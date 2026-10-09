//! 帧剖析：一帧里 CPU 时间花在哪。
//!
//! ```
//! klog::profile::set_enabled(true);
//! {
//!     let _scope = klog::profile!("cull");
//!     // ……
//! }
//! let frame = klog::profile::end_frame();
//! assert!(frame.find("cull").is_some());
//! ```
//!
//! # 设计
//!
//! - **关着的时候几乎零成本**：`profile!` 先看一个原子布尔，关着就返回一个空守卫，
//!   不读时钟、不加锁。所以可以放心插在每帧都走的路径上。
//! - **按调用路径汇总**：同一帧里同一条路径（`frame/render/shadow`）进出多次时
//!   累加时间、计次数。路径靠线程局部的栈记，嵌套自然就有层级。
//! - **只记主线程以外的不丢**：任务池里的线程也能计，它们各自有自己的栈，
//!   路径从它们自己的根开始。
//! - 一帧结束时调用方（kapp）调 [`end_frame`] 取走这一帧的结果并清空。

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static FRAME: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

thread_local! {
    static STACK: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// 一条路径在一帧里的汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// 从根到这一层的名字，例如 `["frame", "render", "shadow"]`。
    pub path: Vec<&'static str>,
    /// 这一帧里累计的时间（含子层）。
    pub total: Duration,
    /// 进了几次。
    pub calls: u32,
}

impl Entry {
    /// 最后一层的名字。
    pub fn name(&self) -> &'static str {
        self.path.last().copied().unwrap_or("")
    }

    /// 层级深度，根是 0。
    pub fn depth(&self) -> usize {
        self.path.len().saturating_sub(1)
    }

    /// 毫秒。
    pub fn ms(&self) -> f64 {
        self.total.as_secs_f64() * 1000.0
    }
}

/// 一帧的剖析结果。条目按**第一次进入的先后**排，父层总在子层前面。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameProfile {
    /// 各条路径。
    pub entries: Vec<Entry>,
}

impl FrameProfile {
    /// 按最后一层名字找第一条（`"cull"` 能找到 `frame/render/cull`）。
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name() == name)
    }

    /// 某个名字在这一帧里的总毫秒数（所有路径加起来），没有就是 0。
    pub fn ms(&self, name: &str) -> f64 {
        self.entries
            .iter()
            .filter(|e| e.name() == name)
            .map(Entry::ms)
            .sum()
    }
}

/// 开关剖析。关着时 [`profile!`](crate::profile) 不计时。
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled && let Ok(mut frame) = FRAME.lock() {
        frame.clear();
    }
}

/// 剖析开着没有。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 取走这一帧的结果并清空，准备下一帧。
pub fn end_frame() -> FrameProfile {
    let entries = FRAME
        .lock()
        .map(|mut f| std::mem::take(&mut *f))
        .unwrap_or_default();
    FrameProfile { entries }
}

/// 计时守卫：活着的这段时间算进它的路径，`drop` 时结算。
#[must_use = "守卫一创建就被丢掉的话计到的时间是 0"]
pub struct Scope {
    start: Option<Instant>,
}

impl Scope {
    /// 进入一层。剖析关着时返回一个什么都不做的守卫。
    pub fn enter(name: &'static str) -> Self {
        if !ENABLED.load(Ordering::Relaxed) {
            return Self { start: None };
        }
        STACK.with(|stack| stack.borrow_mut().push(name));
        Self {
            start: Some(Instant::now()),
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        let elapsed = start.elapsed();
        let path = STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let path = stack.clone();
            stack.pop();
            path
        });
        // 剖析在这一层进去之后被关掉了：丢掉，不往清空了的表里写。
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut frame) = FRAME.lock() else { return };
        // 父层后于子层结束，但要排在子层前面：找不到时插到第一个子层之前。
        match frame.iter_mut().find(|e| e.path == path) {
            Some(entry) => {
                entry.total += elapsed;
                entry.calls += 1;
            }
            None => {
                let position = frame
                    .iter()
                    .position(|e| e.path.len() > path.len() && e.path.starts_with(&path))
                    .unwrap_or(frame.len());
                frame.insert(
                    position,
                    Entry {
                        path,
                        total: elapsed,
                        calls: 1,
                    },
                );
            }
        }
    }
}

/// 一串首尾相接的段：`next("剔除")` 结束上一段、开始这一段。
///
/// 长函数按「── 某某 ──」分成几块时用它，不必为每块包一层作用域。
#[derive(Default)]
pub struct Sequence {
    current: Option<Scope>,
}

impl Sequence {
    /// 还没开始任何一段。
    pub fn new() -> Self {
        Self::default()
    }

    /// 结束当前段（如果有），开始 `name`。
    pub fn next(&mut self, name: &'static str) {
        // 先丢旧的再进新的：守卫的 drop 会弹栈，顺序反了路径就错了。
        self.current = None;
        self.current = Some(Scope::enter(name));
    }

    /// 结束当前段。
    pub fn end(&mut self) {
        self.current = None;
    }
}

/// 计一段代码的时间：`let _t = klog::profile!("cull");`，作用域结束时结算。
#[macro_export]
macro_rules! profile {
    ($name:expr) => {
        $crate::profile::Scope::enter($name)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // 剖析状态是进程全局的，几条测试串起来跑，免得互相清掉对方的数据。
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn nested_scopes_become_a_tree_in_entry_order() {
        let _guard = SERIAL.lock().unwrap();
        set_enabled(true);
        end_frame();
        {
            let _frame = Scope::enter("frame");
            for _ in 0..3 {
                let _cull = Scope::enter("cull");
                std::thread::sleep(Duration::from_millis(1));
            }
            let _draw = Scope::enter("draw");
        }
        let profile = end_frame();
        let names: Vec<(&str, usize, u32)> = profile
            .entries
            .iter()
            .map(|e| (e.name(), e.depth(), e.calls))
            .collect();
        assert_eq!(names, [("frame", 0, 1), ("cull", 1, 3), ("draw", 1, 1)]);
        assert!(profile.ms("cull") >= 3.0);
        assert!(profile.ms("frame") >= profile.ms("cull"), "父层包含子层");
        assert!(end_frame().entries.is_empty(), "取走就清");
        set_enabled(false);
    }

    #[test]
    fn disabled_scopes_record_nothing() {
        let _guard = SERIAL.lock().unwrap();
        set_enabled(false);
        {
            let _a = Scope::enter("ignored");
        }
        set_enabled(true);
        assert!(end_frame().find("ignored").is_none());
        set_enabled(false);
    }
}
