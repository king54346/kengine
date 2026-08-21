//! 模块系统（`require`）的测试。

use crate::{Script, ScriptRuntime};
use kcore::pool::Handle;
use kscene::{Node, Scene};

/// 跑一个脚本一帧，返回它 emit 出来的信号名。
fn run(runtime: &mut ScriptRuntime, source: &str) -> Vec<String> {
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("Test"));
    run_on(runtime, &mut scene, node, source)
}

fn run_on(
    runtime: &mut ScriptRuntime,
    scene: &mut Scene,
    node: Handle<Node>,
    source: &str,
) -> Vec<String> {
    let script = Script::new(source, "test.js");
    let Some(_id) = runtime.instantiate(&script, node) else {
        return Vec::new();
    };
    runtime
        .process(scene, &kasset::ResourceManager::new(), 1.0 / 60.0, 0.0)
        .into_iter()
        .map(|signal| signal.name)
        .collect()
}

#[test]
fn a_script_can_require_a_module() {
    // 脚本之间复用代码——没有这个的话只能把公共函数抄进每个脚本。
    let mut runtime = ScriptRuntime::new();
    assert!(runtime.add_module("utils", "exports.double = (x) => x * 2;"));

    let signals = run(
        &mut runtime,
        r#"
        const utils = require("utils");
        return {
            _ready() { if (utils.double(3) === 6) emit("ok"); },
        };
        "#,
    );
    assert_eq!(signals, vec!["ok"]);
}

#[test]
fn module_exports_can_be_replaced_wholesale() {
    // `module.exports = ...` 是 CommonJS 里导出单个函数/类的常规写法。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("adder", "module.exports = (a, b) => a + b;");

    let signals = run(
        &mut runtime,
        r#"
        const add = require("adder");
        return { _ready() { if (add(2, 3) === 5) emit("ok"); } };
        "#,
    );
    assert_eq!(signals, vec!["ok"]);
}

#[test]
fn a_module_is_only_executed_once() {
    // 第二次 require 该走缓存。不缓存的话模块里的初始化会重复跑，
    // 计数器、注册表这类东西会被重置。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module(
        "counter",
        "globalThis.__runs = (globalThis.__runs || 0) + 1; exports.runs = globalThis.__runs;",
    );

    let signals = run(
        &mut runtime,
        r#"
        const a = require("counter");
        const b = require("counter");
        return {
            _ready() {
                if (a === b && a.runs === 1) emit("cached");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["cached"]);
}

#[test]
fn modules_have_their_own_scope() {
    // 模块顶层的 `let` 不能漏进全局去污染别的脚本。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("scoped", "let secret = 42; exports.get = () => secret;");

    let signals = run(
        &mut runtime,
        r#"
        const m = require("scoped");
        return {
            _ready() {
                if (m.get() !== 42) return;
                if (typeof secret === "undefined") emit("isolated");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["isolated"]);
}

#[test]
fn a_module_can_require_another_module() {
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("base", "exports.value = 10;");
    runtime.add_module(
        "derived",
        "const base = require('base'); exports.value = base.value * 2;",
    );

    let signals = run(
        &mut runtime,
        r#"
        const d = require("derived");
        return { _ready() { if (d.value === 20) emit("ok"); } };
        "#,
    );
    assert_eq!(signals, vec!["ok"]);
}

#[test]
fn requiring_a_missing_module_throws_a_useful_error() {
    // 报错要指名道姓，不然写错一个字母得查半天。
    let mut runtime = ScriptRuntime::new();
    let signals = run(
        &mut runtime,
        r#"
        return {
            _ready() {
                try {
                    require("nope");
                } catch (e) {
                    if (String(e).indexOf("nope") >= 0) emit("named");
                }
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["named"]);
}

#[test]
fn a_broken_module_is_not_left_in_the_cache() {
    // 执行失败的模块留在缓存里的话，第二次 require 会拿到一个空壳，
    // 报出来的错离真正的原因十万八千里。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("broken", "throw new Error('模块炸了');");

    let signals = run(
        &mut runtime,
        r#"
        return {
            _ready() {
                let first = "";
                let second = "";
                try { require("broken"); } catch (e) { first = String(e); }
                try { require("broken"); } catch (e) { second = String(e); }
                // 两次的错误必须一样——第二次拿到空壳的话就不一样了。
                if (first === second && first.indexOf("模块炸了") >= 0) emit("same");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["same"]);
}

#[test]
fn a_circular_dependency_does_not_hang() {
    // 先放进缓存再执行，循环依赖时后来者拿到的是半成品而不是无限递归。
    // 这是 CommonJS 的标准行为——不崩，但拿到的东西可能不完整。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module(
        "a",
        "exports.name = 'a'; const b = require('b'); exports.sawB = b.name;",
    );
    runtime.add_module(
        "b",
        "exports.name = 'b'; const a = require('a'); exports.sawA = a.name;",
    );

    let signals = run(
        &mut runtime,
        r#"
        const a = require("a");
        return {
            _ready() {
                // a 完整地看到了 b；b 看到 a 时 a 还没执行完，
                // 但 exports.name 已经赋过值了，所以能看到。
                if (a.name === "a" && a.sawB === "b") emit("no-hang");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["no-hang"]);
}

#[test]
fn re_registering_a_module_clears_its_cache() {
    // 热重载：同名再注册一次，下次 require 该拿到新版本。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("v", "exports.n = 1;");

    let first = run(
        &mut runtime,
        r#"
        const v = require("v");
        return { _ready() { emit("n" + v.n); } };
        "#,
    );
    assert_eq!(first, vec!["n1"]);

    runtime.add_module("v", "exports.n = 2;");
    let second = run(
        &mut runtime,
        r#"
        const v = require("v");
        return { _ready() { emit("n" + v.n); } };
        "#,
    );
    assert_eq!(second, vec!["n2"], "重新注册之后没拿到新版本");
}

#[test]
fn has_module_reports_registration() {
    let mut runtime = ScriptRuntime::new();
    assert!(!runtime.has_module("x"));
    runtime.add_module("x", "exports.a = 1;");
    assert!(runtime.has_module("x"));
}

#[test]
fn a_module_source_with_quotes_and_backslashes_survives() {
    // 源码是通过对象属性直接写进去的，不是拼一段 JS 去 eval——
    // 拼字符串的话这段源码会拼出语法错误，甚至逃逸成可执行代码。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module(
        "tricky",
        r#"exports.s = "he said \"hi\"\n and a backslash: \\";"#,
    );

    let signals = run(
        &mut runtime,
        r#"
        const t = require("tricky");
        return {
            _ready() {
                if (t.s.indexOf('"hi"') >= 0 && t.s.indexOf("\\") >= 0) emit("intact");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["intact"]);
}

#[test]
fn a_module_injection_attempt_stays_a_string() {
    // 同上的反面：源码里带 `"};` 这种收尾，如果是拼进 eval 的，
    // 就能提前闭合字符串跑出任意代码。
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("inject", r#"exports.ok = true; // "}; globalThis.__pwned = 1;"#);

    let signals = run(
        &mut runtime,
        r#"
        const m = require("inject");
        return {
            _ready() {
                if (m.ok && typeof globalThis.__pwned === "undefined") emit("safe");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["safe"]);
}

#[test]
fn two_runtimes_do_not_share_modules() {
    // 模块存在各自的 Context 里。共用一份线程局部的话，同一条线程上的
    // 两个运行时会互相看到对方的模块——`host.rs` 的第三条不变量
    // 记的就是这类 bug。
    let mut first = ScriptRuntime::new();
    let mut second = ScriptRuntime::new();
    first.add_module("only_in_first", "exports.a = 1;");

    assert!(first.has_module("only_in_first"));
    assert!(!second.has_module("only_in_first"));
}

#[test]
fn engine_require_is_the_same_function() {
    let mut runtime = ScriptRuntime::new();
    runtime.add_module("m", "exports.v = 7;");
    let signals = run(
        &mut runtime,
        r#"
        return {
            _ready() {
                if (engine.require("m").v === 7) emit("ok");
            },
        };
        "#,
    );
    assert_eq!(signals, vec!["ok"]);
}
