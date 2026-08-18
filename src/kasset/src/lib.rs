//! kasset —— 异步资源加载与追踪。
//!
//! 三个核心概念：
//!
//! - [`ResourceData`]：能被当作资源存储的数据，实现它即可接入系统。
//! - [`ResourceLoader`]：把某类文件解析成 [`ResourceData`]，按扩展名注册。
//! - [`ResourceManager`]：请求资源、缓存去重、把加载任务派发到 IO 线程池。
//!
//! [`ResourceManager::request`] 立即返回句柄，加载在后台进行；句柄可以查询状态，
//! 也可以直接 `.await`。
//!
//! ```
//! use kasset::prelude::*;
//! use kcore::uuid::{uuid, Uuid};
//! use std::{path::PathBuf, sync::Arc};
//!
//! // 1. 定义资源类型
//! #[derive(Debug)]
//! struct Text(String);
//!
//! impl ResourceData for Text {
//!     fn type_uuid(&self) -> Uuid {
//!         uuid!("3f2a1c88-5d4e-4b7a-9c1f-2e6d8a0b4f31")
//!     }
//! }
//!
//! // 2. 定义加载器
//! #[derive(Debug)]
//! struct TextLoader;
//!
//! impl ResourceLoader for TextLoader {
//!     fn extensions(&self) -> &[&str] { &["txt"] }
//!     fn data_type_uuid(&self) -> Uuid {
//!         uuid!("3f2a1c88-5d4e-4b7a-9c1f-2e6d8a0b4f31")
//!     }
//!     fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
//!         Box::pin(async move {
//!             let bytes = io.load_file(&path).await?;
//!             let text = String::from_utf8(bytes).map_err(LoadError::custom)?;
//!             Ok(Box::new(Text(text)) as Box<dyn ResourceData>)
//!         })
//!     }
//! }
//!
//! // 3. 注册并请求
//! let io = MemoryResourceIo::new().with("hello.txt", "你好");
//! let manager = ResourceManager::with_io(Arc::new(io));
//! manager.add_loader(TextLoader);
//!
//! let resource = manager.request_blocking::<Text>("hello.txt").unwrap();
//! assert_eq!(resource.data_ref().unwrap().0, "你好");
//! ```

#![warn(missing_docs)]

pub mod error;
pub mod io;
pub mod loader;
pub mod manager;
pub mod resource;
pub mod serialize;
pub mod state;

pub use error::LoadError;
pub use io::{FsResourceIo, MemoryResourceIo, ResourceIo};
pub use loader::{BoxedLoaderFuture, LoaderContainer, LoaderResult, ResourceLoader};
pub use manager::ResourceManager;
pub use resource::{Resource, ResourceData, ResourceDataRef};
pub use serialize::{manager_from, visit_resource, visit_resource_option};
pub use state::{ResourceState, UntypedResource};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        BoxedLoaderFuture, FsResourceIo, LoadError, LoaderResult, MemoryResourceIo, Resource,
        ResourceData, ResourceIo, ResourceLoader, ResourceManager, ResourceState, UntypedResource,
    };
}

#[cfg(test)]
mod test {
    use crate::prelude::*;
    use kcore::uuid::{Uuid, uuid};
    use std::{path::PathBuf, sync::Arc};

    const TEXT_UUID: Uuid = uuid!("3f2a1c88-5d4e-4b7a-9c1f-2e6d8a0b4f31");

    #[derive(Debug, PartialEq)]
    struct Text(String);

    impl ResourceData for Text {
        fn type_uuid(&self) -> Uuid {
            TEXT_UUID
        }
    }

    #[derive(Debug)]
    struct TextLoader;

    impl ResourceLoader for TextLoader {
        fn extensions(&self) -> &[&str] {
            &["txt"]
        }

        fn data_type_uuid(&self) -> Uuid {
            TEXT_UUID
        }

        fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
            Box::pin(async move {
                let bytes = io.load_file(&path).await?;
                let text = String::from_utf8(bytes).map_err(LoadError::custom)?;
                Ok(Box::new(Text(text)) as Box<dyn ResourceData>)
            })
        }
    }

    fn manager_with(files: &[(&str, &str)]) -> ResourceManager {
        let mut io = MemoryResourceIo::new();
        for (path, contents) in files {
            io.add(*path, *contents);
        }
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(TextLoader);
        manager
    }

    #[test]
    fn loads_resource_asynchronously() {
        let manager = manager_with(&[("a.txt", "内容")]);

        let resource = manager.request_blocking::<Text>("a.txt").unwrap();

        assert!(resource.is_ok());
        assert_eq!(resource.data_ref().unwrap().0, "内容");
    }

    #[test]
    fn same_path_returns_same_resource() {
        let manager = manager_with(&[("a.txt", "内容")]);

        let first = manager.request::<Text>("a.txt");
        let second = manager.request::<Text>("a.txt");

        // 两个句柄指向同一份底层数据，不会重复加载。
        assert!(first.untyped().ptr_eq(second.untyped()));
        assert_eq!(manager.total_count(), 1);
    }

    #[test]
    fn missing_loader_fails_immediately() {
        let manager = manager_with(&[("a.bin", "内容")]);

        let resource = manager.request::<Text>("a.bin");

        assert!(resource.is_failed());
        assert!(matches!(
            resource.error(),
            Some(LoadError::NoLoader { .. })
        ));
    }

    #[test]
    fn missing_file_reports_io_error() {
        let manager = manager_with(&[]);

        let error = manager
            .request_blocking::<Text>("nope.txt")
            .expect_err("文件不存在，加载应当失败");

        assert!(matches!(error, LoadError::Io { .. }));
    }

    #[test]
    fn wrong_type_yields_no_data() {
        #[derive(Debug)]
        struct Other;
        impl ResourceData for Other {
            fn type_uuid(&self) -> Uuid {
                uuid!("00000000-0000-4000-8000-000000000001")
            }
        }

        let manager = manager_with(&[("a.txt", "内容")]);
        let resource = manager.request_blocking::<Text>("a.txt").unwrap();

        // 状态是就绪的，但按错误的类型取数据会得到 None，而不是 panic。
        let mistyped = Resource::<Other>::from_untyped(resource.untyped().clone());
        assert!(mistyped.is_ok());
        assert!(mistyped.data_ref().is_none());
    }

    #[test]
    fn awaiting_ready_resource_returns_immediately() {
        let manager = manager_with(&[("a.txt", "内容")]);
        let resource = manager.request_blocking::<Text>("a.txt").unwrap();

        // 已就绪的资源再次 await 不应阻塞。
        let again = ktask::block_on(resource.clone()).unwrap();
        assert_eq!(again.data_ref().unwrap().0, "内容");
    }

    #[test]
    fn registered_data_skips_loading() {
        let manager = manager_with(&[]);

        let resource = manager.register("builtin", Text("内嵌".to_string()));

        assert!(resource.is_ok());
        assert_eq!(resource.data_ref().unwrap().0, "内嵌");
        assert_eq!(manager.loaded_count(), 1);
    }

    #[test]
    fn counters_track_state() {
        let manager = manager_with(&[("a.txt", "1"), ("b.txt", "2")]);

        manager.request_blocking::<Text>("a.txt").unwrap();
        manager.request_blocking::<Text>("b.txt").unwrap();
        let _ = manager.request::<Text>("c.bin"); // 无加载器，立即失败

        assert_eq!(manager.total_count(), 3);
        assert_eq!(manager.loaded_count(), 2);
        assert_eq!(manager.failed_count(), 1);
        assert!(manager.is_idle());
    }

    #[test]
    fn collect_unused_keeps_externally_held_resources() {
        let manager = manager_with(&[("a.txt", "1"), ("b.txt", "2")]);

        let kept = manager.request_blocking::<Text>("a.txt").unwrap();
        manager.request_blocking::<Text>("b.txt").unwrap();

        // b 的句柄已经丢弃，只有 a 还被外部持有。
        let removed = manager.collect_unused();

        assert_eq!(removed, 1);
        assert_eq!(manager.total_count(), 1);
        assert_eq!(kept.data_ref().unwrap().0, "1");
    }

    #[test]
    fn parallel_requests_all_complete() {
        let files: Vec<(String, String)> = (0..32)
            .map(|i| (format!("file{i}.txt"), format!("内容{i}")))
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let manager = manager_with(&borrowed);

        // 一次性发起全部请求，再逐个等待——验证并发加载不会互相干扰。
        let handles: Vec<_> = files
            .iter()
            .map(|(path, _)| manager.request::<Text>(path))
            .collect();

        for (handle, (_, expected)) in handles.into_iter().zip(files.iter()) {
            let ready = ktask::block_on(handle).unwrap();
            assert_eq!(&ready.data_ref().unwrap().0, expected);
        }

        assert_eq!(manager.loaded_count(), 32);
    }
}
