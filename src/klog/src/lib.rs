mod once;

/// 在每个调用点只执行一次表达式。
///
/// 使用 `std::sync::Once` 保证线程安全，常用于"每帧只打印一次"的场景。
#[macro_export]
macro_rules! once {
    ($expression:expr) => {{
        static SHOULD_FIRE: ::std::sync::atomic::AtomicBool =
            ::std::sync::atomic::AtomicBool::new(true);
        if SHOULD_FIRE.swap(false, ::std::sync::atomic::Ordering::Relaxed) {
            $expression;
        }
    }};
}

pub use tracing::{
    self, debug, debug_span, error, error_span, event, info, info_span, trace, trace_span, warn,
    warn_span, Level,
};
pub use tracing_subscriber;

/// 日志前缀模块
pub mod prelude {
    pub use tracing::{debug, error, info, trace, warn};
    pub use crate::{debug_once, error_once, info_once, trace_once, warn_once};
}

/// 默认过滤器：应用自身放行到 `info`，同时屏蔽 wgpu / naga 的冗余日志。
///
/// 开头的 `info` 是**全局默认指令**，不可省略——`EnvFilter` 在没有全局指令时
/// 会把未匹配的 target 默认成 `error`，导致应用自己的 `info!`/`warn!` 被静默丢弃。
pub const DEFAULT_FILTER: &str = "info,wgpu=error,naga=warn";

/// 初始化全局日志订阅器。
///
/// 会读取 `RUST_LOG` 环境变量；若未设置则使用 `default_filter`（或 [`DEFAULT_FILTER`]）。
///
/// # 示例
/// ```no_run
/// klog::init(None); // 使用默认过滤
/// klog::init(Some("debug")); // 自定义过滤级别
/// ```
pub fn init(filter: Option<&str>) {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(filter.unwrap_or(DEFAULT_FILTER)));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// 以指定 [`Level`] 初始化日志，不读取环境变量。
pub fn init_with_level(level: Level) {
    use tracing_subscriber::fmt;
    fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
}