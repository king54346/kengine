//! 脚本资源：一份 JavaScript 源码。
//!
//! # 约定
//!
//! 脚本文件是一个**函数体**，返回一个带生命周期方法的对象：
//!
//! ```js
//! let spin = 1.0;
//!
//! return {
//!     init() {
//!         engine.log("我醒了");
//!     },
//!     update(dt) {
//!         engine.rotateY(engine.self(), spin * dt);
//!     },
//!     destroy() {
//!         engine.log("我走了");
//!     },
//! };
//! ```
//!
//! 三个方法都是可选的。写成函数体（而不是直接一个对象字面量）是为了让
//! 脚本能有自己的**闭包变量**——上面的 `spin` 每个实例各有一份，
//! 而不是所有实例共享一个全局。
//!
//! 源码只解析一次：运行时把它包成一个工厂函数，每实例化一次就调一次工厂。

use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::{Uuid, uuid};
use std::{path::PathBuf, sync::Arc};

/// [`Script`] 的资源类型标识。
pub const SCRIPT_TYPE_UUID: Uuid = uuid!("9b42d1e7-6c85-4a30-b7f9-1e5d83c02a64");

/// 一份脚本源码。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    source: String,
    name: String,
}

impl Script {
    /// 用源码创建，`name` 只用于日志与报错。
    pub fn new(source: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            name: name.into(),
        }
    }

    /// 源码。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// 脚本名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 包成工厂函数的完整源码。
    ///
    /// 换行是必须的：脚本最后一行如果是 `// 注释`，不换行会把闭合的 `}`
    /// 一起注释掉，报出来的语法错误指向一个根本不存在的位置。
    pub fn as_factory(&self) -> String {
        format!("(function(){{\n{}\n}})", self.source)
    }
}

impl ResourceData for Script {
    fn type_uuid(&self) -> Uuid {
        SCRIPT_TYPE_UUID
    }
}

/// [`Script`] 的资源加载器。
#[derive(Debug, Default, Clone, Copy)]
pub struct ScriptLoader;

impl ResourceLoader for ScriptLoader {
    fn extensions(&self) -> &[&str] {
        &["js"]
    }

    fn data_type_uuid(&self) -> Uuid {
        SCRIPT_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let source = String::from_utf8(bytes).map_err(LoadError::custom)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            Ok(Box::new(Script::new(source, name)) as Box<dyn ResourceData>)
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kasset::{MemoryResourceIo, ResourceManager};

    #[test]
    fn a_script_keeps_its_source_and_name() {
        let script = Script::new("return {};", "spin.js");

        assert_eq!(script.source(), "return {};");
        assert_eq!(script.name(), "spin.js");
    }

    #[test]
    fn the_factory_wrapper_puts_the_body_on_its_own_line() {
        // 脚本最后一行是注释时，不换行会把闭合的 `}` 一起注释掉。
        let script = Script::new("return {}; // 收尾", "x.js");
        let factory = script.as_factory();

        assert!(factory.starts_with("(function(){\n"));
        assert!(factory.ends_with("\n})"));
    }

    #[test]
    fn the_loader_reads_source_through_the_resource_manager() {
        let io = MemoryResourceIo::new().with("spin.js", b"return { update(dt) {} };".to_vec());
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(ScriptLoader);

        let resource = manager.request_blocking::<Script>("spin.js").unwrap();
        let script = resource.data_ref().unwrap();

        assert_eq!(script.name(), "spin.js");
        assert!(script.source().contains("update"));
    }

    #[test]
    fn the_loader_claims_the_js_extension() {
        assert!(ScriptLoader.extensions().contains(&"js"));
    }

    #[test]
    fn a_non_utf8_file_fails_the_resource_rather_than_the_process() {
        let io = MemoryResourceIo::new().with("bad.js", vec![0xff, 0xfe, 0x00]);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(ScriptLoader);

        assert!(manager.request_blocking::<Script>("bad.js").is_err());
    }
}
