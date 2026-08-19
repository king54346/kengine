//! kscript —— JavaScript 脚本。
//!
//! 脚本挂在场景节点上，每帧收到 `update(dt)`。引擎 API 通过全局 `engine`
//! 对象暴露：`engine.self()`、`engine.find(name)`、`engine.rotateY(node, a)`……
//!
//! ```
//! use kscript::{Script, ScriptRuntime, Snapshot, NodeRef, Command};
//!
//! let mut runtime = ScriptRuntime::new();
//! let script = Script::new("return { update(dt) { engine.rotateY(engine.self(), dt); } };", "spin.js");
//! runtime.instantiate(&script, NodeRef(0)).unwrap();
//!
//! let commands = runtime.tick(Snapshot::new(0.5, 0.0));
//! assert_eq!(commands, vec![Command::RotateY(NodeRef(0), 0.5)]);
//! ```
//!
//! # 架构：快照进、命令出
//!
//! 脚本**不直接访问场景**。每帧把它关心的状态拍成快照递进去，脚本产生一串
//! 命令，返回后由引擎依次落地。理由与代价见 [`api`] 的模块文档。

#![warn(missing_docs)]

pub mod api;
mod runtime;
mod script;

pub use api::{Command, CommandBuffer, NodeRef, NodeState, Snapshot};
pub use runtime::{InstanceId, ScriptRuntime, ScriptStats};
pub use script::{SCRIPT_TYPE_UUID, Script, ScriptLoader};
