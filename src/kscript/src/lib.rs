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
//! # 异步、信号、树（照 GDScript）
//!
//! ```js
//! return {
//!     async _ready() {
//!         await wait(1.0);                          // 游戏时间，暂停时跟着停
//!         const door = self.getNode("../Door");     // 相对路径；"/Level/Door" 从根开始
//!         door.connect("opened", by => console.log("门被", by, "打开了"));
//!         const [who] = await getNode("Boss").toSignal("died");
//!         self.rotationDegrees = new Vector3(0, 90, 0);
//!         setInterval(() => self.rotateY(0.1), 100);   // 毫秒，和浏览器一致
//!     },
//! };
//! ```
//!
//! - `self` 是工厂参数，绑定在实例自己的节点上：`await` 之后、计时器和信号
//!   回调里都不会变成别人。
//! - 计时器最早在登记之后的下一帧触发，到点的在当帧 `_process` 全部跑完之后执行。
//! - `async` 生命周期方法、计时器、信号回调里抛的异常照样停掉**出错的那个**脚本，
//!   报错带文件名和行号（`enemy.js:12:5`）。
//! - 脚本停掉或节点删掉时，它的计时器和信号订阅一并作废。
//! - 热重载时实现了 `_save` / `_load` 的脚本会带着状态换代码。
//! - 另有 `console.*`（[`ScriptRuntime::take_console`] 取走）、`Quaternion`、
//!   `Mathf`、可设种子的 `RandomNumberGenerator`。
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
mod bridge_ext;
mod host;
mod runtime;
mod script;

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod async_tests;
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
#[cfg(test)]
mod transform_tests;

pub use runtime::{
    ConsoleLevel, ConsoleMessage, InstanceId, ScriptError, ScriptRuntime, ScriptStats, Signal,
};
pub use script::{SCRIPT_TYPE_UUID, Script, ScriptLoader};
