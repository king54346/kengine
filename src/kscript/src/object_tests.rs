//! 对象驱动那一层的三件事：读输入、生成节点、脚本之间互相调用。
//!
//! 这三样合起来才够写一个真正由脚本驱动的游戏——少任何一个，逻辑就得
//! 退回 Rust 侧，脚本沦为「改改数值」的配置文件。

use crate::{Script, ScriptRuntime};
use kasset::{MemoryResourceIo, ResourceManager};
use kinput::{Input, KeyCode};
use kmath::Vec3;
use kscene::{Node, Scene};
use std::sync::Arc;

/// 建一个装好脚本加载器、内存里放着若干脚本的资源管理器。
fn resources(scripts: &[(&str, &str)]) -> ResourceManager {
    let mut io = MemoryResourceIo::new();
    for (path, source) in scripts {
        io.add(*path, source.as_bytes().to_vec());
    }
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    for (path, _) in scripts {
        let _ = manager.request_blocking::<Script>(*path);
    }
    manager
}

/// 跑一帧 `_process`，返回信号名。
fn tick(
    runtime: &mut ScriptRuntime,
    scene: &mut Scene,
    input: &mut Input,
    manager: &ResourceManager,
) -> Vec<String> {
    scene.update();
    runtime
        .process(scene, input, manager, 1.0 / 60.0, 0.0)
        .into_iter()
        .map(|signal| signal.name)
        .collect()
}

// ── 输入 ──

#[test]
fn a_script_reads_actions_and_axes() {
    let manager = resources(&[(
        "s.js",
        r#"
        return {
            _process() {
                if (Input.pressed("jump")) emit("held");
                if (Input.justPressed("jump")) emit("fresh");
                self.position.x = Input.axis("move_x");
            },
        };
        "#,
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("s.js"));

    let mut input = Input::new();
    input.bindings_mut().bind_action("jump", KeyCode::Space);
    input
        .bindings_mut()
        .bind_axis("move_x", KeyCode::KeyD, KeyCode::KeyA);
    input.press_key(KeyCode::Space);
    input.press_key(KeyCode::KeyD);

    let first = tick(&mut runtime, &mut scene, &mut input, &manager);
    assert_eq!(
        first,
        ["held", "fresh"],
        "第一帧该同时是「按住」和「刚按下」"
    );
    assert_eq!(scene[node].transform.position.x, 1.0, "轴没读到");

    // 一帧过去，「刚按下」该消失，「按住」还在——这条正是每帧清理的边界。
    input.end_frame();
    let second = tick(&mut runtime, &mut scene, &mut input, &manager);
    assert_eq!(second, ["held"]);
}

#[test]
fn the_input_comes_back_after_the_tick() {
    // 输入是搬进线程局部再搬回来的，漏搬的话调用方手里就只剩个空壳，
    // 这一帧之后所有按键查询都会变成 false——静默失效，最难查的那种。
    let manager = resources(&[("s.js", "return { _process() {} };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let mut input = Input::new();
    input.bindings_mut().bind_action("jump", KeyCode::Space);
    input.press_key(KeyCode::Space);

    tick(&mut runtime, &mut scene, &mut input, &manager);

    assert!(input.action_pressed("jump"), "输入没搬回来");
}

#[test]
fn an_unbound_action_is_simply_not_pressed() {
    // 脚本里写了个不存在的动作名不该炸，也不该恒为真。
    let manager = resources(&[(
        "s.js",
        r#"return { _process() { if (Input.pressed("nope")) emit("wrong"); } };"#,
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let signals = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);

    assert!(signals.is_empty());
}

// ── 生成 ──

#[test]
fn a_script_spawns_a_registered_prototype() {
    let manager = resources(&[(
        "s.js",
        r#"
        return {
            _ready() {
                const e = spawn("Enemy", new Vector3(1, 2, 3));
                emit(e === null ? "null" : e.name);
            },
        };
        "#,
    )]);
    let mut runtime = ScriptRuntime::new();
    runtime.register_prototype("Enemy", || Node::new("Enemy"));

    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let signals = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);

    assert_eq!(signals, ["Enemy"], "生成的节点当场就该能读名字");
    let spawned = scene.find_by_name("Enemy").expect("场景里没有生成的节点");
    assert_eq!(scene[spawned].transform.position, Vec3::new(1.0, 2.0, 3.0));
}

#[test]
fn a_spawned_node_runs_its_own_script_from_the_next_frame() {
    // 生成的节点当帧就在场景里，但它的脚本要等下一次 process 才实例化。
    // 这是个真实的一帧延迟，写在文档里也测在这里。
    let manager = resources(&[
        (
            "spawner.js",
            r#"return { _ready() { spawn("Bullet", Vector3.ZERO()); } };"#,
        ),
        (
            "bullet.js",
            r#"return { _ready() { emit("bullet-ready"); } };"#,
        ),
    ]);
    let mut runtime = ScriptRuntime::new();
    runtime.register_prototype("Bullet", || Node::new("Bullet").with_script("bullet.js"));

    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("spawner.js"));

    let first = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);
    assert!(first.is_empty(), "子弹的 _ready 不该在生成的那一帧就跑");

    let second = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);
    assert_eq!(second, ["bullet-ready"]);
}

#[test]
fn spawning_an_unknown_prototype_returns_null() {
    let manager = resources(&[(
        "s.js",
        r#"
        return {
            _ready() {
                const x = spawn("Typo", Vector3.ZERO());
                emit(x === null ? "null" : "something");
            },
        };
        "#,
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let signals = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);

    assert_eq!(signals, ["null"]);
}

#[test]
fn a_runaway_spawn_loop_is_capped_instead_of_eating_all_memory() {
    // `for(;;) spawn(...)` 每次都往场景里塞一个节点，比攒信号吃内存快得多。
    let manager = resources(&[(
        "s.js",
        r#"
        return {
            _ready() {
                let made = 0;
                for (let i = 0; i < 5000; i++) {
                    if (spawn("Blob", Vector3.ZERO()) !== null) made++;
                }
                emit("made", made);
            },
        };
        "#,
    )]);
    let mut runtime = ScriptRuntime::new();
    runtime.register_prototype("Blob", || Node::new("Blob"));

    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    scene.update();
    let signals = runtime.process(&mut scene, &mut Input::new(), &manager, 1.0 / 60.0, 0.0);

    assert_eq!(signals[0].value, ScriptRuntime::MAX_SPAWNS_PER_TICK as f64);
}

#[test]
fn the_spawn_budget_refills_every_tick() {
    // 上限是「每次 tick」，不是「一辈子」——否则跑够几分钟之后
    // 游戏就再也生不出东西了。
    let manager = resources(&[(
        "s.js",
        r#"return { _process() { spawn("Blob", Vector3.ZERO()); } };"#,
    )]);
    let mut runtime = ScriptRuntime::new();
    runtime.register_prototype("Blob", || Node::new("Blob"));

    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    for _ in 0..3 {
        tick(&mut runtime, &mut scene, &mut Input::new(), &manager);
    }

    let blobs = scene
        .nodes()
        .iter()
        .filter(|node| node.name == "Blob")
        .count();
    assert_eq!(blobs, 3, "每帧都该生成得出来");
}

// ── 脚本之间互相调用 ──

#[test]
fn one_script_calls_a_method_on_another() {
    let manager = resources(&[
        (
            "player.js",
            r#"return { _ready() { getNode("Bag").script.add(3); } };"#,
        ),
        (
            "bag.js",
            r#"
            return {
                items: 0,
                add(n) { this.items += n; emit("items", this.items); },
            };
            "#,
        ),
    ]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    // 背包**先**加进场景：实例化按场景里的顺序来，玩家 `_ready` 跑的时候
    // 背包的实例必须已经在 `__instances` 里了。
    scene.add_node(Node::new("Bag").with_script("bag.js"));
    scene.add_node(Node::new("Player").with_script("player.js"));

    let signals = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);

    assert_eq!(signals, ["items"], "跨脚本调用没到达");
}

#[test]
fn a_node_without_a_script_has_none() {
    let manager = resources(&[(
        "s.js",
        r#"
        return {
            _ready() {
                emit(getNode("Plain").script === null ? "null" : "something");
            },
        };
        "#,
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("Plain"));
    scene.add_node(Node::new("n").with_script("s.js"));

    let signals = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);

    assert_eq!(signals, ["null"]);
}

#[test]
fn a_dead_nodes_script_is_no_longer_reachable() {
    // 节点删掉之后，别的脚本不该还能摸到它的对象——那是个挂在坟头上的实例，
    // 调它的方法会对着失效句柄写场景，写不进去也不报错。
    let manager = resources(&[
        ("ghost.js", "return { alive() { return true; } };"),
        (
            "watcher.js",
            r#"
            let target = null;
            return {
                _ready() { target = getNode("Ghost"); },
                _process() {
                    if (target === null) return;
                    emit(target.script === null ? "gone" : "still-there");
                },
            };
            "#,
        ),
    ]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let ghost = scene.add_node(Node::new("Ghost").with_script("ghost.js"));
    scene.add_node(Node::new("Watcher").with_script("watcher.js"));

    let first = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);
    assert_eq!(first, ["still-there"]);

    scene.remove_node(ghost);
    // 第一帧收掉死实例，第二帧才看得到结果——回收发生在遍历到它的时候，
    // 而 watcher 可能排在它前面。
    tick(&mut runtime, &mut scene, &mut Input::new(), &manager);
    let after = tick(&mut runtime, &mut scene, &mut Input::new(), &manager);

    assert_eq!(after, ["gone"]);
}

#[test]
fn prototypes_survive_a_scene_switch() {
    // `clear` 是切场景时调的。原型是游戏侧登记的，跟场景没关系——
    // 每换一关都要重新登记一遍的话，每个换关卡的地方都得记着这件事。
    let mut runtime = ScriptRuntime::new();
    runtime.register_prototype("Enemy", || Node::new("Enemy"));

    runtime.clear();

    assert_eq!(runtime.prototypes().collect::<Vec<_>>(), ["Enemy"]);
}
