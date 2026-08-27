//! kscript —— JavaScript 脚本，接口照 GDScript。
//!
//! 脚本挂在场景节点上，**实时读写场景**：写下去立刻生效，还能当场打射线。
//!
//! ```js
//! let speed = 2.0;
//!
//! return {
//!     _ready() {
//!         print("我醒了：", self.name);
//!     },
//!
//!     _process(delta) {
//!         self.position.y += speed * delta;          // 写进去立刻生效
//!         const hit = raycast(self.globalPosition, Vector3.DOWN(), 5.0);
//!         if (hit) print("脚下 ", hit.distance, " 米是 ", hit.node.name);
//!     },
//!
//!     _physics_process(delta) {
//!         self.applyImpulse(Vector3.UP().mul(delta));  // delta 恒等于物理步长
//!     },
//! };
//! ```
//!
//! # 分层
//!
//! `kscene` **不认识**脚本引擎，节点上只有一个存路径的槽位
//! （[`kscene::ScriptSlot`]，因此脚本能随场景存档）。反过来 kscript 依赖
//! kscene——脚本要实时读写场景。boa 只有这个 crate 认识。
//!
//! # 实时访问怎么做到的（零 `unsafe`）
//!
//! tick 期间把整个 `Scene` 用 `mem::swap` 搬进线程局部，跑完再搬回来。
//! 细节与两条不变量见 `host` 模块。

#![warn(missing_docs)]

mod bridge;
mod host;
mod runtime;
mod script;

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod debug_tests;
#[cfg(test)]
mod module_tests;
#[cfg(test)]
mod object_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tests;

pub use runtime::{InstanceId, ScriptError, ScriptRuntime, ScriptStats, Signal};
pub use script::{SCRIPT_TYPE_UUID, Script, ScriptLoader};
