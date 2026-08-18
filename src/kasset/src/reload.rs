//! 热重载：改了磁盘上的资源，运行中的游戏立刻跟着变。
//!
//! # 为什么是轮询而不是文件系统事件
//!
//! 用 `notify` 那类库监听 inotify / ReadDirectoryChanges 当然更即时，
//! 但代价是一个不小的依赖、一个后台线程，以及各平台事件语义的差异
//! （一次保存在不同编辑器下可能触发 1 到 4 个事件）。
//!
//! 这里改成**按修改时间轮询**：只看已经被请求过的那几十上百个资源，
//! 每秒查一次 `mtime`。代价是最多晚一个轮询周期，换来的是零依赖、
//! 跨平台行为一致、而且能在测试里精确控制时序。资源多到轮询成为瓶颈时
//! 再换实现也不迟——[`HotReload`] 的接口不会因此改变。
//!
//! # 只在开发时开
//!
//! 发布版的资源在包里，改不了也不该改。[`HotReload`] 只对
//! 「路径确实指向一个能 stat 到的文件」的资源生效，包里的资源自然被跳过。

use crate::manager::ResourceManager;
use fxhash::FxHashMap;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// 资源热重载的看门人。
///
/// 每帧调用 [`poll`](Self::poll)，它自己按间隔节流。
pub struct HotReload {
    manager: ResourceManager,
    interval: Duration,
    next_poll: Instant,
    /// 上一次看到的修改时间。没进这张表的路径视为「第一次见」，不触发重载。
    stamps: FxHashMap<PathBuf, SystemTime>,
    enabled: bool,
}

impl std::fmt::Debug for HotReload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotReload")
            .field("interval", &self.interval)
            .field("watched", &self.stamps.len())
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl HotReload {
    /// 默认的轮询间隔。
    pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(1000);

    /// 盯住一个资源管理器。
    ///
    /// 建立时会把当前所有资源的修改时间记下来当基线，所以刚创建的这一瞬间
    /// 不会把整批资源判定成「变了」。
    pub fn new(manager: &ResourceManager) -> Self {
        let mut watcher = Self {
            manager: manager.clone(),
            interval: Self::DEFAULT_INTERVAL,
            next_poll: Instant::now() + Self::DEFAULT_INTERVAL,
            stamps: FxHashMap::default(),
            enabled: true,
        };
        watcher.snapshot();
        watcher
    }

    /// 指定轮询间隔。
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self.next_poll = Instant::now() + interval;
        self
    }

    /// 是否启用。
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 开关。关掉之后 [`poll`](Self::poll) 直接返回空。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// 当前盯着多少个文件。
    pub fn watched_count(&self) -> usize {
        self.stamps.len()
    }

    /// 把当前所有资源的修改时间记成基线，不触发任何重载。
    pub fn snapshot(&mut self) {
        for path in self.manager.paths() {
            if let Some(stamp) = modified_time(&path) {
                self.stamps.insert(path, stamp);
            }
        }
    }

    /// 到点就查一遍，返回这一轮重新加载了哪些资源。
    ///
    /// 没到点、或已关闭时返回空 `Vec`，可以放心每帧调用。
    pub fn poll(&mut self) -> Vec<PathBuf> {
        if !self.enabled || Instant::now() < self.next_poll {
            return Vec::new();
        }
        self.next_poll = Instant::now() + self.interval;
        self.poll_now()
    }

    /// 立刻查一遍，忽略间隔节流。测试与「手动触发重载」用。
    pub fn poll_now(&mut self) -> Vec<PathBuf> {
        let mut reloaded = Vec::new();

        for path in self.manager.paths() {
            let Some(stamp) = modified_time(&path) else {
                // stat 不到：资源在包里、或者文件被删了。
                // 删文件不触发重载——重载只会把好好的资源变成加载失败。
                continue;
            };

            match self.stamps.get(&path) {
                Some(previous) if *previous == stamp => {}
                Some(_) => {
                    self.stamps.insert(path.clone(), stamp);
                    if self.manager.reload(&path) {
                        reloaded.push(path);
                    }
                }
                // 第一次见到这个资源（刚被请求），只记基线，不重载。
                None => {
                    self.stamps.insert(path, stamp);
                }
            }
        }

        reloaded
    }
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{BoxedLoaderFuture, ResourceData, ResourceIo, ResourceLoader};
    use kcore::uuid::{Uuid, uuid};
    use std::sync::Arc;

    #[derive(Debug, Default)]
    struct Note(String);

    const NOTE_UUID: Uuid = uuid!("b0d4e7a2-5c31-4f88-9a6e-2d70f1c93b45");

    impl ResourceData for Note {
        fn type_uuid(&self) -> Uuid {
            NOTE_UUID
        }
    }

    struct NoteLoader;

    impl ResourceLoader for NoteLoader {
        fn extensions(&self) -> &[&str] {
            &["note"]
        }
        fn data_type_uuid(&self) -> Uuid {
            NOTE_UUID
        }
        fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
            Box::pin(async move {
                let bytes = io.load_file(&path).await?;
                Ok(Box::new(Note(String::from_utf8_lossy(&bytes).into_owned()))
                    as Box<dyn ResourceData>)
            })
        }
    }

    /// 一个临时目录 + 一个装了 `NoteLoader` 的管理器。
    fn stage(name: &str) -> (PathBuf, ResourceManager) {
        let directory = std::env::temp_dir().join(format!("kengine_reload_{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();

        let manager = ResourceManager::new();
        manager.add_loader(NoteLoader);
        (directory, manager)
    }

    /// 改文件内容，并保证修改时间确实往前走了一格。
    ///
    /// 有些文件系统的 `mtime` 精度只有 1 秒或 10 毫秒，写得太快会让
    /// 前后两次修改的时间戳完全相同，测试就会变成随机通过。
    fn rewrite(path: &Path, contents: &str) {
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(path, contents).unwrap();
        let now = std::time::SystemTime::now() + Duration::from_secs(1);
        let _ = filetime_touch(path, now);
    }

    /// 把修改时间显式往后推，绕开文件系统的时间精度问题。
    fn filetime_touch(path: &Path, time: SystemTime) -> std::io::Result<()> {
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.set_modified(time)?;
        Ok(())
    }

    fn content(resource: &crate::Resource<Note>) -> String {
        resource.data_ref().map(|d| d.0.clone()).unwrap_or_default()
    }

    /// 等重载落地。重载是丢给 IO 池的异步任务，不等就会读到「加载中」。
    fn wait_ready(resource: &crate::Resource<Note>) {
        for _ in 0..2000 {
            if !resource.is_loading() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("重载迟迟没有完成");
    }

    #[test]
    fn a_changed_file_is_reloaded_in_place() {
        // 热重载的全部意义：已有的句柄不用换，内容自己变。
        let (directory, manager) = stage("changed");
        let path = directory.join("a.note");
        std::fs::write(&path, "before").unwrap();

        let resource = manager.request_blocking::<Note>(&path).unwrap();
        assert_eq!(content(&resource), "before");

        let mut watcher = HotReload::new(&manager);
        rewrite(&path, "after");

        let reloaded = watcher.poll_now();
        assert_eq!(reloaded.len(), 1, "文件变了却没触发重载");

        wait_ready(&resource);
        assert_eq!(content(&resource), "after", "句柄没看到新内容");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_untouched_file_is_not_reloaded() {
        let (directory, manager) = stage("untouched");
        let path = directory.join("a.note");
        std::fs::write(&path, "stable").unwrap();
        manager.request_blocking::<Note>(&path).unwrap();

        let mut watcher = HotReload::new(&manager);

        assert!(watcher.poll_now().is_empty());
        assert!(watcher.poll_now().is_empty(), "重复轮询不该反复重载");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_first_sighting_of_a_resource_only_records_a_baseline() {
        // 刚请求进来的资源不该立刻被重载一遍——那是纯浪费。
        let (directory, manager) = stage("baseline");
        let path = directory.join("a.note");
        std::fs::write(&path, "x").unwrap();

        let mut watcher = HotReload::new(&manager);
        assert_eq!(watcher.watched_count(), 0);

        manager.request_blocking::<Note>(&path).unwrap();

        assert!(watcher.poll_now().is_empty(), "第一次见就重载了");
        assert_eq!(watcher.watched_count(), 1);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_deleted_file_does_not_trigger_a_failing_reload() {
        // 重载一个已经不存在的文件，只会把好好的资源变成加载失败。
        let (directory, manager) = stage("deleted");
        let path = directory.join("a.note");
        std::fs::write(&path, "here").unwrap();
        let resource = manager.request_blocking::<Note>(&path).unwrap();

        let mut watcher = HotReload::new(&manager);
        std::fs::remove_file(&path).unwrap();

        assert!(watcher.poll_now().is_empty());
        assert_eq!(content(&resource), "here", "资源被删文件牵连了");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn resources_that_are_not_files_are_skipped() {
        // 包里的资源 stat 不到，轮询该安静跳过而不是报错。
        use crate::{MemoryResourceIo, ResourceManager};

        let io = MemoryResourceIo::new().with("in_memory.note", b"x".to_vec());
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(NoteLoader);
        manager.request_blocking::<Note>("in_memory.note").unwrap();

        let mut watcher = HotReload::new(&manager);

        assert_eq!(watcher.watched_count(), 0);
        assert!(watcher.poll_now().is_empty());
    }

    #[test]
    fn polling_is_throttled_by_the_interval() {
        let (directory, manager) = stage("throttle");
        let path = directory.join("a.note");
        std::fs::write(&path, "before").unwrap();
        manager.request_blocking::<Note>(&path).unwrap();

        let mut watcher = HotReload::new(&manager).with_interval(Duration::from_secs(3600));
        rewrite(&path, "after");

        // 间隔还没到，`poll` 什么都不做；`poll_now` 则无视节流。
        assert!(watcher.poll().is_empty());
        assert_eq!(watcher.poll_now().len(), 1);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_disabled_watcher_does_nothing() {
        let (directory, manager) = stage("disabled");
        let path = directory.join("a.note");
        std::fs::write(&path, "before").unwrap();
        manager.request_blocking::<Note>(&path).unwrap();

        let mut watcher = HotReload::new(&manager).with_interval(Duration::ZERO);
        watcher.set_enabled(false);
        rewrite(&path, "after");

        assert!(watcher.poll().is_empty());
        assert!(!watcher.is_enabled());

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn reloading_an_unknown_path_is_a_no_op() {
        let manager = ResourceManager::new();
        assert!(!manager.reload("never/requested.note"));
    }

    #[test]
    fn the_same_handle_survives_several_reloads() {
        let (directory, manager) = stage("repeat");
        let path = directory.join("a.note");
        std::fs::write(&path, "v0").unwrap();
        let resource = manager.request_blocking::<Note>(&path).unwrap();
        let mut watcher = HotReload::new(&manager);

        for version in 1..4 {
            rewrite(&path, &format!("v{version}"));
            assert_eq!(watcher.poll_now().len(), 1, "第 {version} 次没重载");
            wait_ready(&resource);
            assert_eq!(content(&resource), format!("v{version}"));
        }

        let _ = std::fs::remove_dir_all(&directory);
    }
}
