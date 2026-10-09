//! 给本包的可执行文件（例子、测试、基准）把主线程栈开到 8 MB。
//!
//! # 为什么
//!
//! Windows 的主线程栈只有 **1 MB**（Linux 是 8 MB）。kscript 用的 boa 在解析和
//! 编译 JavaScript 时递归很深：实测光是一个空的 `Context` 求值 `1 + 2` 就要
//! 两百多 KB，表达式每多嵌一层括号再加一百多 KB——引擎自己的前奏脚本加上
//! 主线程原本的调用链，正好顶破 1 MB，表现为一启动就 `STATUS_STACK_OVERFLOW`
//! （`physics_character` 就是这样挂的）。用户脚本里写一个稍深的表达式，
//! 在加载那一刻也会同样崩掉。
//!
//! boa 已经按 `opt-level = 3` 编译了（见 `Cargo.toml` 的 `profile.dev.package."*"`），
//! 栈用量是它的解析器本来的样子，不是调试构建的问题——所以修的是栈的大小，
//! 把 Windows 拉到和 Linux 一样。
//!
//! 只影响链接这一步：改这个文件不会让依赖重新编译。
//!
//! 用引擎写自己的游戏时，可执行文件归你的包管，这个设置带不过去——
//! 在你自己的 `build.rs` 里照抄这几行即可（见 `kapp` 的文档）。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    const STACK: u32 = 8 * 1024 * 1024;
    let argument = if target.contains("msvc") {
        format!("/STACK:{STACK}")
    } else {
        format!("-Wl,--stack,{STACK}")
    };
    for kind in ["bins", "examples", "tests", "benches"] {
        println!("cargo:rustc-link-arg-{kind}={argument}");
    }
}
