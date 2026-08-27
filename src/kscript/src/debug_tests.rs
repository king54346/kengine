//! 脚本调试支持的测试。
//!
//! 范围说明：boa 撑不起真正的断点单步调试器（没有 `debugger` 语句支持、
//! 没有调试协议）。这里做的是三件够用的事：
//!
//! 1. 挂掉的实例**留下第一条错误**，随时能查
//! 2. 从实例反查节点和脚本名
//! 3. 在实例的上下文里**求值一个表达式**（REPL 式）

use crate::{Script, ScriptRuntime};
use kasset::{MemoryResourceIo, ResourceManager};
use kscene::{Node, Scene};
use std::sync::Arc;

fn setup(source: &str) -> (Scene, ResourceManager, ScriptRuntime) {
    let io = MemoryResourceIo::new().with("s.js", source.as_bytes().to_vec());
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    let _ = manager.request_blocking::<Script>("s.js");

    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));
    scene.update();
    (scene, manager, ScriptRuntime::new())
}

fn tick(runtime: &mut ScriptRuntime, scene: &mut Scene, manager: &ResourceManager) {
    scene.update();
    let mut input = kinput::Input::new();
    runtime.process(scene, &mut input, manager, 1.0 / 60.0, 0.0);
}

fn only_instance(scene: &Scene) -> crate::InstanceId {
    let handle = scene.find_by_name("n").expect("节点");
    let slot = scene.try_get(handle).unwrap().script().expect("脚本槽位");
    crate::InstanceId(slot.instance)
}

// ── 错误留痕 ──

#[test]
fn a_failing_script_records_its_error() {
    let (mut scene, manager, mut runtime) =
        setup("return { _process(dt) { throw new Error('炸了'); } };");
    tick(&mut runtime, &mut scene, &manager);

    let id = only_instance(&scene);
    let error = runtime.error(id).expect("该记下错误");

    assert_eq!(error.method, "_process");
    assert_eq!(error.script, "s.js");
    assert!(
        error.message.contains("炸了"),
        "错误信息丢了：{}",
        error.message
    );
}

#[test]
fn only_the_first_error_is_kept() {
    // 脚本坏掉之后往往每帧都报同样的错。留最后一条的话，日志刷屏
    // 之后真正的第一条就找不着了。
    let (mut scene, manager, mut runtime) = setup(
        r#"
        let n = 0;
        return {
            _process(dt) { n += 1; throw new Error("第" + n + "次"); },
        };
        "#,
    );
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);
    let first = runtime.error(id).unwrap().message.clone();

    // 再跑几帧。脚本已经被停用，不该有新错误覆盖。
    for _ in 0..5 {
        tick(&mut runtime, &mut scene, &manager);
    }

    assert_eq!(runtime.error(id).unwrap().message, first);
    assert!(first.contains("第1次"), "留的不是第一条：{first}");
}

#[test]
fn a_healthy_script_has_no_error() {
    let (mut scene, manager, mut runtime) = setup("return { _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);

    assert!(runtime.error(only_instance(&scene)).is_none());
    assert!(runtime.errors().is_empty());
}

#[test]
fn errors_lists_every_broken_instance() {
    // 调试面板一次列全。
    let io = MemoryResourceIo::new()
        .with(
            "bad.js",
            b"return { _ready() { throw new Error('x'); } };".to_vec(),
        )
        .with("good.js", b"return { _ready() {} };".to_vec());
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    let _ = manager.request_blocking::<Script>("bad.js");
    let _ = manager.request_blocking::<Script>("good.js");

    let mut scene = Scene::new();
    scene.add_node(Node::new("a").with_script("bad.js"));
    scene.add_node(Node::new("b").with_script("bad.js"));
    scene.add_node(Node::new("c").with_script("good.js"));

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let errors = runtime.errors();
    assert_eq!(errors.len(), 2, "该有两个挂掉的实例");
    assert!(errors.iter().all(|(_, e)| e.script == "bad.js"));
}

#[test]
fn a_ready_error_is_attributed_to_ready() {
    // 方法名要对得上，不然查起来根本不知道该看哪一段。
    let (mut scene, manager, mut runtime) = setup("return { _ready() { throw new Error('x'); } };");
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(
        runtime.error(only_instance(&scene)).unwrap().method,
        "_ready"
    );
}

#[test]
fn reviving_clears_the_failure() {
    // 改完脚本热重载之后用。
    let (mut scene, manager, mut runtime) = setup(
        r#"
        let boom = true;
        return {
            _process(dt) { if (boom) { boom = false; throw new Error('一次'); } emit("活了"); },
        };
        "#,
    );
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);
    assert!(runtime.is_failed(id));

    assert!(runtime.revive(id));
    assert!(!runtime.is_failed(id));
    assert!(runtime.error(id).is_none());

    scene.update();
    let signals = runtime.process(
        &mut scene,
        &mut kinput::Input::new(),
        &manager,
        1.0 / 60.0,
        0.0,
    );
    assert_eq!(signals.len(), 1, "复活之后该继续跑");
}

#[test]
fn reviving_a_missing_instance_returns_false() {
    let mut runtime = ScriptRuntime::new();
    assert!(!runtime.revive(crate::InstanceId(99)));
}

// ── 反查 ──

#[test]
fn an_instance_can_be_traced_back_to_its_node_and_script() {
    let (mut scene, manager, mut runtime) = setup("return { _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);

    let id = only_instance(&scene);
    let node = runtime.node_of(id).expect("该能反查节点");

    assert_eq!(scene.try_get(node).unwrap().name, "n");
    assert_eq!(runtime.name_of(id), Some("s.js"));
}

#[test]
fn tracing_a_missing_instance_returns_none() {
    let runtime = ScriptRuntime::new();
    assert!(runtime.node_of(crate::InstanceId(99)).is_none());
    assert!(runtime.name_of(crate::InstanceId(99)).is_none());
}

// ── REPL ──

#[test]
fn an_expression_can_be_evaluated_in_an_instance() {
    let (mut scene, manager, mut runtime) = setup("return { hp: 80, _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);

    assert_eq!(runtime.eval_in(id, &mut scene, "this.hp").unwrap(), "80");
    assert_eq!(runtime.eval_in(id, &mut scene, "1 + 2").unwrap(), "3");
}

#[test]
fn the_expression_sees_the_scene() {
    // `self`、`getNode`、`raycast` 都要能用，否则 REPL 只能算算术。
    let (mut scene, manager, mut runtime) = setup("return { _process(dt) {} };");
    let handle = scene.find_by_name("n").unwrap();
    scene.try_get_mut(handle).unwrap().transform.position.y = 3.5;
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);

    assert_eq!(runtime.eval_in(id, &mut scene, "self.name").unwrap(), "n");
    assert_eq!(
        runtime.eval_in(id, &mut scene, "self.position.y").unwrap(),
        "3.5"
    );
}

#[test]
fn the_expression_binds_this_to_the_right_instance() {
    // 两个实例各有各的 `this`。绑错的话查到的是别人的状态。
    let io = MemoryResourceIo::new().with(
        "s.js",
        // `tag` 在 `_ready` 里取，不在顶层——顶层跑的时候 `self` 还没绑定。
        b"return { tag: null, _ready() { this.tag = self.name; } };".to_vec(),
    );
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    let _ = manager.request_blocking::<Script>("s.js");

    let mut scene = Scene::new();
    scene.add_node(Node::new("first").with_script("s.js"));
    scene.add_node(Node::new("second").with_script("s.js"));

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let ids: Vec<_> = ["first", "second"]
        .iter()
        .map(|name| {
            let h = scene.find_by_name(name).unwrap();
            crate::InstanceId(scene.try_get(h).unwrap().script().unwrap().instance)
        })
        .collect();

    assert_eq!(
        runtime.eval_in(ids[0], &mut scene, "this.tag").unwrap(),
        "first"
    );
    assert_eq!(
        runtime.eval_in(ids[1], &mut scene, "this.tag").unwrap(),
        "second"
    );
}

#[test]
fn a_broken_expression_returns_an_error_not_a_panic() {
    let (mut scene, manager, mut runtime) = setup("return { _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);

    assert!(
        runtime
            .eval_in(id, &mut scene, "这不是合法表达式 (((")
            .is_err()
    );
    assert!(runtime.eval_in(id, &mut scene, "undefinedThing.x").is_err());
    // 出错之后运行时还能继续用。
    assert_eq!(runtime.eval_in(id, &mut scene, "42").unwrap(), "42");
}

#[test]
fn evaluating_on_a_missing_instance_is_an_error() {
    let (mut scene, _manager, mut runtime) = setup("return {};");
    assert!(
        runtime
            .eval_in(crate::InstanceId(99), &mut scene, "1")
            .is_err()
    );
}

#[test]
fn the_scene_is_returned_after_evaluation() {
    // 场景是搬进线程局部再搬回来的。搬回来这一步漏了的话，
    // 调用方拿到的是个空壳，整个游戏静默失效。
    let (mut scene, manager, mut runtime) = setup("return { _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);

    let before = scene.nodes().alive_count();
    let _ = runtime.eval_in(id, &mut scene, "self.name");
    assert_eq!(scene.nodes().alive_count(), before, "场景没被搬回来");

    // 出错的那条路径也要搬回来。
    let _ = runtime.eval_in(id, &mut scene, "throw new Error('x')");
    assert_eq!(scene.nodes().alive_count(), before);

    // 搬回来之后还能正常跑。
    tick(&mut runtime, &mut scene, &manager);
}

#[test]
fn an_expression_can_write_to_the_scene() {
    // 这不是沙箱，写下去立刻生效。文档里写明了这一点。
    let (mut scene, manager, mut runtime) = setup("return { _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);

    runtime
        .eval_in(id, &mut scene, "self.position = new Vector3(1, 2, 3)")
        .unwrap();

    let handle = scene.find_by_name("n").unwrap();
    assert_eq!(scene.try_get(handle).unwrap().transform.position.y, 2.0);
}

#[test]
fn self_is_not_bound_while_the_factory_runs() {
    // 脚本**顶层**（`return` 之前那段）跑在实例化阶段，那时还没有
    // 「当前节点」的概念。
    //
    // `self` 本身不是 null——它是个 id 无效的 `Node`（`selfId()` 返回 -1）。
    // 表现是 `self.valid === false`、`self.name === null`。
    //
    // 这一条容易踩：`const name = self.name;` 写在顶层不报错，
    // 只是静默拿到 null，等到 `_ready` 里用的时候才莫名其妙。
    // 要用 `self` 就写在生命周期方法里。
    let (mut scene, manager, mut runtime) = setup(
        r#"
        const nameAtTopLevel = self.name;
        const validAtTopLevel = self.valid;
        return {
            _ready() {
                emit("top-name:" + nameAtTopLevel);
                emit("top-valid:" + validAtTopLevel);
                emit("ready-valid:" + self.valid);
            },
        };
        "#,
    );
    scene.update();
    let signals: Vec<String> = runtime
        .process(
            &mut scene,
            &mut kinput::Input::new(),
            &manager,
            1.0 / 60.0,
            0.0,
        )
        .into_iter()
        .map(|s| s.name)
        .collect();

    assert_eq!(
        signals,
        vec!["top-name:null", "top-valid:false", "ready-valid:true"]
    );
}

#[test]
fn closure_variables_are_not_reachable() {
    // 明确记下这个局限：`let hp = 100;` 活在闭包里，从外面够不着。
    // 要能查就得挂到 `this` 上。
    let (mut scene, manager, mut runtime) = setup("let hidden = 5; return { _process(dt) {} };");
    tick(&mut runtime, &mut scene, &manager);
    let id = only_instance(&scene);

    assert!(
        runtime.eval_in(id, &mut scene, "hidden").is_err(),
        "闭包变量竟然能从外面读到"
    );
}
