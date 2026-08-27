//! 脚本状态存读的测试。
//!
//! 核心约定：脚本的**闭包变量存不下来**。`let hp = 100;` 活在 JS 的闭包里，
//! Rust 这边够不着，而且里面可能有函数、有循环引用。所以状态是显式的——
//! 脚本实现 `_save()` 和 `_load(state)`。

use crate::{Script, ScriptRuntime};
use kasset::{MemoryResourceIo, ResourceManager};
use kscene::{Node, Scene};
use std::sync::Arc;

/// 造一个挂着脚本的场景，脚本源码从内存资源里取。
fn scene_with_script(source: &str) -> (Scene, ResourceManager) {
    let io = MemoryResourceIo::new().with("s.js", source.as_bytes().to_vec());
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    // 先把脚本加载完再开跑。不预热的话资源还在异步解码，
    // 一帧之内脚本一个都建不起来，测出来全是空结果。
    let _ = manager.request_blocking::<Script>("s.js");

    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));
    scene.update();
    (scene, manager)
}

fn tick(runtime: &mut ScriptRuntime, scene: &mut Scene, manager: &ResourceManager) -> Vec<String> {
    scene.update();
    runtime
        .process(scene, &mut kinput::Input::new(), manager, 1.0 / 60.0, 0.0)
        .into_iter()
        .map(|s| s.name)
        .collect()
}

/// 一个把计数存进状态的脚本。
const COUNTER: &str = r#"
let count = 0;
return {
    _process(dt) { count += 1; },
    _save() { return { count: count }; },
    _load(state) { count = state.count; },
    _ready() { emit("start:" + count); },
};
"#;

#[test]
fn a_script_state_survives_save_and_load() {
    let (mut scene, manager) = scene_with_script(COUNTER);
    let mut runtime = ScriptRuntime::new();

    // 跑三帧，计数涨到 3。
    for _ in 0..3 {
        tick(&mut runtime, &mut scene, &manager);
    }

    // 存档前把状态收回节点上。
    assert_eq!(runtime.save_states(&mut scene), 1);
    let bytes = scene.save_to_vec().expect("存档");

    // 读档，换一个全新的运行时。
    let mut loaded = Scene::load_from_slice(&bytes, None).expect("读档");
    let mut fresh = ScriptRuntime::new();
    let signals = tick(&mut fresh, &mut loaded, &manager);

    assert_eq!(signals, vec!["start:3"], "状态没被喂回去，脚本从 0 开始了");
}

#[test]
fn state_is_delivered_before_ready() {
    // `_ready` 里多半要根据状态决定做什么（按存下来的血量决定是不是
    // 该播死亡动画）。喂晚了它看到的是初始值。
    let (mut scene, manager) = scene_with_script(COUNTER);
    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);
    runtime.save_states(&mut scene);
    let bytes = scene.save_to_vec().unwrap();

    let mut loaded = Scene::load_from_slice(&bytes, None).unwrap();
    let mut fresh = ScriptRuntime::new();
    let signals = tick(&mut fresh, &mut loaded, &manager);

    // `_ready` 只跑一次，它看到的必须已经是读回来的值。
    assert_eq!(signals, vec!["start:1"]);
}

#[test]
fn a_script_without_save_has_no_state() {
    // 不实现 `_save` 的脚本不该在存档里占位置。
    let (mut scene, manager) = scene_with_script("return { _process(dt) {} };");
    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(runtime.save_states(&mut scene), 0);
    let handle = scene.find_by_name("n").unwrap();
    assert!(
        scene
            .try_get(handle)
            .unwrap()
            .script()
            .unwrap()
            .state
            .is_empty()
    );
}

#[test]
fn a_script_slot_survives_save_even_without_state() {
    // 这一条记录的是一个真实的 bug：`ScriptSlot` 原来**根本没有被序列化**，
    // 存档读回来之后节点上的脚本就没了。而 kscript 的文档一直写着
    // 「因此脚本能随场景存档」。
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("spin.js"));

    let bytes = scene.save_to_vec().unwrap();
    let loaded = Scene::load_from_slice(&bytes, None).unwrap();
    let handle = loaded.find_by_name("n").unwrap();
    let slot = loaded
        .try_get(handle)
        .unwrap()
        .script()
        .expect("脚本槽位没能存活");

    assert_eq!(slot.path, "spin.js");
    assert!(slot.enabled);
}

#[test]
fn runtime_state_is_not_serialized() {
    // `instance` 是运行时状态。存下来的话读档后会指向上一次运行里的
    // 实例编号——那个编号在新的运行时里要么不存在，要么是别人的。
    let (mut scene, manager) = scene_with_script(COUNTER);
    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let handle = scene.find_by_name("n").unwrap();
    let live = scene.try_get(handle).unwrap().script().unwrap().instance;
    assert_ne!(
        live,
        kscene::ScriptSlot::NO_INSTANCE,
        "脚本没实例化，这条测试没意义"
    );

    let bytes = scene.save_to_vec().unwrap();
    let loaded = Scene::load_from_slice(&bytes, None).unwrap();
    let handle = loaded.find_by_name("n").unwrap();
    let slot = loaded.try_get(handle).unwrap().script().unwrap();

    assert_eq!(slot.instance, kscene::ScriptSlot::NO_INSTANCE);
    assert!(!slot.failed);
}

#[test]
fn a_save_that_cannot_be_serialized_is_skipped() {
    // 循环引用的对象 `JSON.stringify` 会抛异常。那个脚本的状态留空，
    // 不影响别人，也不该让整个存档失败。
    let (mut scene, manager) = scene_with_script(
        r#"
        return {
            _save() {
                const a = {};
                a.self = a;      // 循环引用
                return a;
            },
        };
        "#,
    );
    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(runtime.save_states(&mut scene), 0);
    // 存档本身仍然要成功。
    assert!(scene.save_to_vec().is_ok());
}

#[test]
fn a_save_returning_undefined_is_skipped() {
    // `_save` 忘了 return 时返回 undefined，`JSON.stringify(undefined)`
    // 给的也是 undefined 而不是字符串。
    let (mut scene, manager) = scene_with_script("return { _save() {} };");
    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(runtime.save_states(&mut scene), 0);
}

#[test]
fn a_throwing_save_does_not_break_the_others() {
    // 一个坏脚本不该让整个存档失败。
    let io = MemoryResourceIo::new()
        .with(
            "bad.js",
            b"return { _save() { throw new Error('x'); } };".to_vec(),
        )
        .with(
            "good.js",
            b"let n = 7; return { _save() { return { n: n }; } };".to_vec(),
        );
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    let _ = manager.request_blocking::<Script>("bad.js");
    let _ = manager.request_blocking::<Script>("good.js");

    let mut scene = Scene::new();
    scene.add_node(Node::new("bad").with_script("bad.js"));
    scene.add_node(Node::new("good").with_script("good.js"));

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(runtime.save_states(&mut scene), 1, "好脚本的状态该存下来");
    let good = scene.find_by_name("good").unwrap();
    assert!(
        scene
            .try_get(good)
            .unwrap()
            .script()
            .unwrap()
            .state
            .contains("7"),
        "好脚本的状态没存对"
    );
}

#[test]
fn a_corrupt_state_does_not_crash_the_script() {
    // 手改过的存档、或者版本对不上的存档。脚本该照常起来，
    // 只是拿不到状态。
    let (mut scene, manager) = scene_with_script(COUNTER);
    let handle = scene.find_by_name("n").unwrap();
    scene
        .try_get_mut(handle)
        .unwrap()
        .script_mut()
        .unwrap()
        .state = "{ 这不是 JSON".to_string();

    let mut runtime = ScriptRuntime::new();
    let signals = tick(&mut runtime, &mut scene, &manager);

    // `_ready` 照跑，计数是初始值。
    assert_eq!(signals, vec!["start:0"]);
}

#[test]
fn state_with_a_missing_load_is_ignored() {
    // 脚本改过了，把 `_load` 删了却留着旧存档。不该崩，也不该静默——
    // 运行时会记一条日志。
    let (mut scene, manager) = scene_with_script("return { _ready() { emit(\"up\"); } };");
    let handle = scene.find_by_name("n").unwrap();
    scene
        .try_get_mut(handle)
        .unwrap()
        .script_mut()
        .unwrap()
        .state = r#"{"count":5}"#.to_string();

    let mut runtime = ScriptRuntime::new();
    assert_eq!(tick(&mut runtime, &mut scene, &manager), vec!["up"]);
}

#[test]
fn saving_twice_overwrites_rather_than_accumulates() {
    let (mut scene, manager) = scene_with_script(COUNTER);
    let mut runtime = ScriptRuntime::new();

    tick(&mut runtime, &mut scene, &manager);
    runtime.save_states(&mut scene);
    let handle = scene.find_by_name("n").unwrap();
    let first = scene
        .try_get(handle)
        .unwrap()
        .script()
        .unwrap()
        .state
        .clone();

    tick(&mut runtime, &mut scene, &manager);
    runtime.save_states(&mut scene);
    let second = scene
        .try_get(handle)
        .unwrap()
        .script()
        .unwrap()
        .state
        .clone();

    assert_ne!(first, second, "第二次存档没更新状态");
    assert!(second.contains('2'));
}

#[test]
fn nested_state_round_trips() {
    // 状态不止是标量：数组、嵌套对象、字符串都要能过去。
    let (mut scene, manager) = scene_with_script(
        r#"
        let data = { items: [1, 2, 3], nested: { name: "阿强" }, flag: true };
        return {
            _save() { return data; },
            _load(s) { data = s; },
            _ready() {
                emit(data.items.length + ":" + data.nested.name + ":" + data.flag);
            },
        };
        "#,
    );
    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);
    runtime.save_states(&mut scene);
    let bytes = scene.save_to_vec().unwrap();

    let mut loaded = Scene::load_from_slice(&bytes, None).unwrap();
    let mut fresh = ScriptRuntime::new();
    assert_eq!(tick(&mut fresh, &mut loaded, &manager), vec!["3:阿强:true"]);
}
