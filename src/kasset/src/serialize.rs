//! 资源引用的序列化。
//!
//! 资源**按路径引用**存盘，而不是把内容抄进去：一张 2K 贴图有几 MB，
//! 一个场景引用它十次不该让场景文件涨十倍。读回来时按路径重新请求，
//! 缓存去重会保证同一个路径只加载一次。
//!
//! 读取那一侧需要一个 [`ResourceManager`]。它通过 Visitor 的
//! **blackboard** 递进来——这正是 blackboard 存在的理由：
//! 给反序列化过程提供它自己造不出来的「全局」东西。
//!
//! ```no_run
//! # use kasset::ResourceManager;
//! # use kcore::visitor::Visitor;
//! # use std::sync::Arc;
//! let manager = ResourceManager::new();
//! let mut visitor = Visitor::new();
//! visitor.blackboard.register(Arc::new(manager));
//! // 之后 visitor 就能把资源引用解析回 Resource<T> 了
//! ```

use crate::{Resource, ResourceData, manager::ResourceManager};
use kcore::visitor::{Visit, VisitResult, Visitor, error::VisitError};
use std::path::PathBuf;

/// 从 Visitor 的 blackboard 里取资源管理器。
///
/// 取不到时给出一条能直接照做的错误，而不是让上层拿到一个空资源、
/// 到渲染时才发现贴图全丢了。
pub fn manager_from(visitor: &Visitor) -> Result<ResourceManager, VisitError> {
    visitor
        .blackboard
        .get::<ResourceManager>()
        .cloned()
        .ok_or_else(|| {
            VisitError::User(
                "反序列化资源引用需要 ResourceManager：\
                 请先 visitor.blackboard.register(Arc::new(manager.clone()))"
                    .to_string(),
            )
        })
}

/// 读写一个可选的资源引用。
///
/// 写入时存路径，读取时按路径重新请求。资源本身仍是异步加载的——
/// 这里拿到的是句柄，不是数据。
pub fn visit_resource_option<T: ResourceData>(
    name: &str,
    slot: &mut Option<Resource<T>>,
    visitor: &mut Visitor,
) -> VisitResult {
    let mut region = visitor.enter_region(name)?;

    let mut present = slot.is_some();
    present.visit("Present", &mut region)?;

    if !present {
        if region.is_reading() {
            *slot = None;
        }
        return Ok(());
    }

    let mut path = slot
        .as_ref()
        .map(|resource| resource.path().to_path_buf())
        .unwrap_or_default();
    path.visit("Path", &mut region)?;

    if region.is_reading() {
        let manager = manager_from(&region)?;
        *slot = Some(manager.request::<T>(path));
    }

    Ok(())
}

/// 读写一个必填的资源引用。
pub fn visit_resource<T: ResourceData>(
    name: &str,
    resource: &mut Resource<T>,
    visitor: &mut Visitor,
) -> VisitResult {
    let mut region = visitor.enter_region(name)?;

    let mut path: PathBuf = resource.path().to_path_buf();
    path.visit("Path", &mut region)?;

    if region.is_reading() {
        let manager = manager_from(&region)?;
        *resource = manager.request::<T>(path);
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{BoxedLoaderFuture, LoadError, MemoryResourceIo, ResourceIo, ResourceLoader};
    use kcore::uuid::{Uuid, uuid};
    use std::sync::Arc;

    #[derive(Debug, Default)]
    struct Note(#[allow(dead_code)] String);

    const NOTE_UUID: Uuid = uuid!("2f5b8c11-4e93-4a27-9d60-71c8f3a4e5b2");

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
                let text = String::from_utf8(bytes).map_err(LoadError::custom)?;
                Ok(Box::new(Note(text)) as Box<dyn ResourceData>)
            })
        }
    }

    fn manager() -> ResourceManager {
        let io = MemoryResourceIo::new().with("a.note", b"hello".to_vec());
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(NoteLoader);
        manager
    }

    /// 存进 Visitor 再读回来，读取端带上资源管理器。
    fn roundtrip(slot: &mut Option<Resource<Note>>, manager: &ResourceManager) -> Option<Resource<Note>> {
        let mut visitor = Visitor::new();
        visit_resource_option("Slot", slot, &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        visitor.blackboard.register(Arc::new(manager.clone()));
        let mut restored = None;
        visit_resource_option("Slot", &mut restored, &mut visitor).unwrap();
        restored
    }

    #[test]
    fn a_resource_reference_survives_as_its_path() {
        let manager = manager();
        let mut slot = Some(manager.request::<Note>("a.note"));

        let restored = roundtrip(&mut slot, &manager).expect("引用丢了");

        assert_eq!(restored.path(), std::path::Path::new("a.note"));
    }

    #[test]
    fn the_reloaded_reference_hits_the_cache_instead_of_loading_twice() {
        // 存的是路径，读回来走的是同一套缓存——同一个路径必须还是同一份资源。
        let manager = manager();
        let original = manager.request::<Note>("a.note");
        let mut slot = Some(original.clone());

        let restored = roundtrip(&mut slot, &manager).unwrap();

        assert!(
            restored.untyped().ptr_eq(original.untyped()),
            "同一路径读回来却成了另一份资源，缓存没起作用"
        );
    }

    #[test]
    fn an_empty_slot_reads_back_empty() {
        let manager = manager();
        let mut slot: Option<Resource<Note>> = None;

        assert!(roundtrip(&mut slot, &manager).is_none());
    }

    #[test]
    fn reading_without_a_manager_fails_loudly() {
        // 悄悄给个空资源的话，问题要到渲染时才暴露成「贴图全丢了」。
        let manager = manager();
        let mut slot = Some(manager.request::<Note>("a.note"));

        let mut visitor = Visitor::new();
        visit_resource_option("Slot", &mut slot, &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored: Option<Resource<Note>> = None;
        let result = visit_resource_option("Slot", &mut restored, &mut visitor);

        assert!(result.is_err(), "没有资源管理器却成功读出了引用");
    }

    #[test]
    fn writing_never_needs_a_manager() {
        // 存盘不该要求把管理器塞进 blackboard，只有读取才需要。
        let manager = manager();
        let mut slot = Some(manager.request::<Note>("a.note"));
        let mut visitor = Visitor::new();

        assert!(visit_resource_option("Slot", &mut slot, &mut visitor).is_ok());
    }

    #[test]
    fn a_required_reference_also_roundtrips() {
        let manager = manager();
        let mut resource = manager.request::<Note>("a.note");

        let mut visitor = Visitor::new();
        visit_resource("R", &mut resource, &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        visitor.blackboard.register(Arc::new(manager.clone()));
        let mut restored = manager.request::<Note>("unrelated.note");
        visit_resource("R", &mut restored, &mut visitor).unwrap();

        assert_eq!(restored.path(), std::path::Path::new("a.note"));
    }
}
