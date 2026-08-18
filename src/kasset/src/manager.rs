//! 资源管理器：请求、缓存、异步加载资源。

use crate::{
    error::LoadError,
    io::{FsResourceIo, ResourceIo},
    loader::{LoaderContainer, ResourceLoader},
    resource::{Resource, ResourceData},
    state::UntypedResource,
};
use fxhash::FxHashMap;
use ktask::{IoTaskPool, TaskPool};
use parking_lot::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

struct ManagerState {
    loaders: LoaderContainer,
    /// 路径 → 资源句柄。同一路径重复请求会命中同一份资源。
    resources: FxHashMap<PathBuf, UntypedResource>,
    io: Arc<dyn ResourceIo>,
}

/// 资源管理器。
///
/// 克隆是廉价的，所有克隆共享同一份缓存与加载器注册表，可自由传递到各个系统。
///
/// ```
/// use kasset::prelude::*;
///
/// let manager = ResourceManager::new();
/// // 注册加载器后即可请求资源：
/// // let texture: Resource<Texture> = manager.request("assets/wall.png");
/// assert_eq!(manager.loaded_count(), 0);
/// ```
#[derive(Clone)]
pub struct ResourceManager {
    state: Arc<Mutex<ManagerState>>,
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceManager {
    /// 用本地文件系统创建管理器。
    pub fn new() -> Self {
        Self::with_io(Arc::new(FsResourceIo))
    }

    /// 指定 IO 后端创建管理器。
    pub fn with_io(io: Arc<dyn ResourceIo>) -> Self {
        // 资源加载跑在全局 IO 池上；首次使用时按需初始化。
        IoTaskPool::get_or_init(TaskPool::default);

        Self {
            state: Arc::new(Mutex::new(ManagerState {
                loaders: LoaderContainer::new(),
                resources: FxHashMap::default(),
                io,
            })),
        }
    }

    /// 注册一个资源加载器。
    pub fn add_loader(&self, loader: impl ResourceLoader) {
        self.state.lock().loaders.add(loader);
    }

    /// 请求一个资源。
    ///
    /// 立即返回句柄，加载在后台进行——句柄一开始处于「加载中」，
    /// 完成后自动变为就绪或失败。同一路径重复请求返回同一个句柄，不会重复加载。
    pub fn request<T: ResourceData>(&self, path: impl AsRef<Path>) -> Resource<T> {
        Resource::from_untyped(self.request_untyped(path))
    }

    /// 与 [`ResourceManager::request`] 相同，但返回非类型化句柄。
    pub fn request_untyped(&self, path: impl AsRef<Path>) -> UntypedResource {
        let path = path.as_ref().to_path_buf();
        let mut state = self.state.lock();

        if let Some(existing) = state.resources.get(&path) {
            return existing.clone();
        }

        let resource = UntypedResource::new_pending(path.clone());
        state.resources.insert(path.clone(), resource.clone());

        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();

        let Some(loader) = state.loaders.find(&extension) else {
            klog::warn!("没有能处理 `{extension}` 的加载器：{}", path.display());
            resource.commit(Err(LoadError::NoLoader { path, extension }));
            return resource;
        };

        let io = state.io.clone();
        drop(state); // 加载任务不需要持有管理器锁。

        let task_resource = resource.clone();
        IoTaskPool::get()
            .spawn(async move {
                let result = loader.load(path.clone(), io).await;
                match &result {
                    Ok(_) => klog::debug!("资源加载完成：{}", path.display()),
                    Err(error) => klog::error!("资源加载失败 {}：{error}", path.display()),
                }
                task_resource.commit(result);
            })
            .detach();

        resource
    }

    /// 阻塞等待资源加载完成。主要用于启动阶段与测试，游戏循环里不要用。
    pub fn request_blocking<T: ResourceData>(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Resource<T>, LoadError> {
        ktask::block_on(self.request::<T>(path))
    }

    /// 重新加载一个已经请求过的资源。
    ///
    /// 数据**就地替换**：所有已存在的 [`Resource<T>`] 句柄立刻看到新内容，
    /// 不需要通知任何人、也不需要重建引用它的场景。这是热重载能成立的前提。
    ///
    /// 返回 `false` 表示这个路径根本没被请求过——重载一个没人要的资源没有意义。
    ///
    /// 重载期间该资源会短暂回到「加载中」状态，此时读取会拿到 `None`；
    /// 调用方的代码本来就要能应付资源尚未就绪（首次加载也是异步的），
    /// 所以这里不额外做双缓冲。
    pub fn reload(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref().to_path_buf();
        let state = self.state.lock();

        let Some(resource) = state.resources.get(&path).cloned() else {
            return false;
        };

        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let Some(loader) = state.loaders.find(&extension) else {
            return false;
        };

        let io = state.io.clone();
        drop(state);

        resource.reset_to_pending();

        let task_resource = resource;
        IoTaskPool::get()
            .spawn(async move {
                let result = loader.load(path.clone(), io).await;
                match &result {
                    Ok(_) => klog::info!("资源已重新加载：{}", path.display()),
                    Err(error) => klog::error!("资源重新加载失败 {}：{error}", path.display()),
                }
                task_resource.commit(result);
            })
            .detach();

        true
    }

    /// 已登记的所有资源路径。
    pub fn paths(&self) -> Vec<std::path::PathBuf> {
        self.state.lock().resources.keys().cloned().collect()
    }

    /// 把一份现成数据登记为资源，跳过加载流程。
    pub fn register<T: ResourceData>(&self, path: impl AsRef<Path>, data: T) -> Resource<T> {
        let path = path.as_ref().to_path_buf();
        let resource = UntypedResource::new_ok(path.clone(), Box::new(data));
        self.state.lock().resources.insert(path, resource.clone());
        Resource::from_untyped(resource)
    }

    /// 缓存中的资源总数（含加载中与失败的）。
    pub fn total_count(&self) -> usize {
        self.state.lock().resources.len()
    }

    /// 已成功加载的资源数量。
    pub fn loaded_count(&self) -> usize {
        self.state
            .lock()
            .resources
            .values()
            .filter(|r| r.is_ok())
            .count()
    }

    /// 仍在加载中的资源数量。
    pub fn pending_count(&self) -> usize {
        self.state
            .lock()
            .resources
            .values()
            .filter(|r| r.is_loading())
            .count()
    }

    /// 加载失败的资源数量。
    pub fn failed_count(&self) -> usize {
        self.state
            .lock()
            .resources
            .values()
            .filter(|r| r.is_failed())
            .count()
    }

    /// 是否所有请求过的资源都已加载完毕（无论成功与否）。
    pub fn is_idle(&self) -> bool {
        self.pending_count() == 0
    }

    /// 从缓存中移除只被管理器自己持有的资源，返回移除数量。
    ///
    /// 外部仍持有句柄的资源不会被移除。
    pub fn collect_unused(&self) -> usize {
        let mut state = self.state.lock();
        let before = state.resources.len();
        // use_count == 1 意味着只有缓存这一个引用。
        state.resources.retain(|_, r| r.use_count() > 1);
        before - state.resources.len()
    }

    /// 清空缓存。
    pub fn clear(&self) {
        self.state.lock().resources.clear();
    }
}
