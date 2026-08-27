//! kscript 的集成测试：脚本 ↔ 场景的完整往返。

use crate::{Script, ScriptRuntime, Signal};
use kasset::{MemoryResourceIo, ResourceManager};
use kmath::Vec3;
use kscene::{Collider, Node, RigidBody, Scene};
use std::sync::Arc;

/// 建一个装好脚本加载器、内存里放着若干脚本的资源管理器。
fn resources(scripts: &[(&str, &str)]) -> ResourceManager {
    let mut io = MemoryResourceIo::new();
    for (path, source) in scripts {
        io.add(*path, source.as_bytes().to_vec());
    }
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);

    // 先把脚本加载完再开跑。真实游戏里这发生在关卡加载阶段；
    // 测试里不预热的话，几帧就跑完了，资源还在异步解码，脚本一个都建不起来。
    for (path, _) in scripts {
        let _ = manager.request_blocking::<Script>(*path);
    }
    manager
}

/// 跑 `frames` 帧 `_process`。
fn process(
    runtime: &mut ScriptRuntime,
    scene: &mut Scene,
    manager: &ResourceManager,
    frames: usize,
    dt: f32,
) -> Vec<Signal> {
    let mut all = Vec::new();
    for frame in 0..frames {
        scene.update();
        all.extend(runtime.process(
            scene,
            &mut kinput::Input::new(),
            manager,
            dt,
            frame as f32 * dt,
        ));
    }
    all
}

// ── GDScript 式的属性读写 ──

#[test]
fn writing_a_single_axis_writes_through_to_the_scene() {
    // 这是 GDScript 最标志性的一行。返回临时副本的话，`.y += 1` 改的是副本，
    // 写完就丢——脚本看起来在动，物体纹丝不动，而且不报错。
    let manager = resources(&[(
        "a.js",
        "return { _process(dt) { self.position.y += 1.0; } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("mover").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 3, 0.016);

    assert_eq!(scene[node].transform.position, Vec3::Y * 3.0);
}

#[test]
fn assigning_a_whole_vector_works_too() {
    let manager = resources(&[(
        "a.js",
        "return { _process() { self.position = new Vector3(1, 2, 3); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("mover").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(scene[node].transform.position, Vec3::new(1.0, 2.0, 3.0));
}

#[test]
fn reads_see_writes_made_earlier_in_the_same_callback() {
    // 实时访问的定义：这一行写下去，下一行就读得到。
    // 旧的「快照进、命令出」架构里这里会读到旧值。
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            self.position.x = 5;
            emit('read_back', self.position.x);
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].value, 5.0, "同一次回调里没读到刚写的值");
}

#[test]
fn scale_and_visibility_round_trip() {
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            self.scale = new Vector3(2, 2, 2);
            self.scale.y = 5;
            self.visible = false;
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(scene[node].transform.scale, Vec3::new(2.0, 5.0, 2.0));
    assert!(!scene[node].visible);
}

#[test]
fn the_node_name_is_readable() {
    let manager = resources(&[("a.js", "return { _ready() { emit(self.name, 1); } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("hero").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals[0].name, "hero");
}

#[test]
fn global_position_accounts_for_the_parent() {
    let manager = resources(&[(
        "a.js",
        "return { _process() { emit('y', self.globalPosition.y); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let parent = scene.add_node(Node::new("parent").with_position(Vec3::Y * 10.0));
    scene.add_node_with_parent(
        Node::new("child")
            .with_position(Vec3::Y * 3.0)
            .with_script("a.js"),
        parent,
    );

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert!(
        (signals[0].value - 13.0).abs() < 1e-4,
        "世界坐标不对：{}",
        signals[0].value
    );
}

// ── 生命周期 ──

#[test]
fn ready_runs_once_before_the_first_process() {
    let manager = resources(&[(
        "a.js",
        "return { _ready() { emit('ready', 1); }, _process() { emit('process', 1); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 3, 0.016);
    let names: Vec<&str> = signals.iter().map(|s| s.name.as_str()).collect();

    assert_eq!(names, vec!["ready", "process", "process", "process"]);
}

#[test]
fn physics_process_is_separate_from_process() {
    let manager = resources(&[(
        "a.js",
        "return {
            _process() { emit('p', 1); },
            _physics_process() { emit('fp', 1); },
        };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    scene.update();
    let mut input = kinput::Input::new();
    runtime.process(&mut scene, &mut input, &manager, 0.016, 0.0);
    // 一帧里跑两个物理子步。
    let a = runtime.physics_process(&mut scene, &mut input, 1.0 / 60.0, 0.0);
    let b = runtime.physics_process(&mut scene, &mut input, 1.0 / 60.0, 0.0);

    assert_eq!(a.len(), 1);
    assert_eq!(a[0].name, "fp");
    assert_eq!(b.len(), 1, "第二个子步也该跑一次");
}

#[test]
fn each_instance_keeps_its_own_closure_state() {
    // 写成函数体而不是对象字面量，就是为了这个。
    let manager = resources(&[(
        "a.js",
        "let n = 0; return { _process() { n += 1; emit('n', n); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("a").with_script("a.js"));
    scene.add_node(Node::new("b").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 2, 0.016);
    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    // 两个实例各自数到 3，而不是一个数到 6。
    assert_eq!(signals.len(), 2);
    assert!(signals.iter().all(|s| s.value == 3.0), "{signals:?}");
}

// ── 跨节点 ──

#[test]
fn get_node_returns_a_usable_object() {
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            const target = getNode('target');
            target.position.x = 7;
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("driver").with_script("a.js"));
    let target = scene.add_node(Node::new("target"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(scene[target].transform.position.x, 7.0);
}

#[test]
fn a_missing_node_comes_back_as_null() {
    // GDScript 里 `get_node` 找不到给 null，不是抛异常。
    let manager = resources(&[(
        "a.js",
        "return { _process() { emit('missing', getNode('nobody') === null ? 1 : 0); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals[0].value, 1.0);
}

#[test]
fn queue_free_removes_the_node_immediately() {
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            const victim = getNode('victim');
            if (victim) {
                victim.queueFree();
                emit('still_valid', victim.valid ? 1 : 0);
            }
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("killer").with_script("a.js"));
    let victim = scene.add_node(Node::new("victim"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert!(scene.try_get(victim).is_none(), "节点没被删掉");
    assert_eq!(signals[0].value, 0.0, "删掉之后 valid 该立刻变 false");
}

#[test]
fn a_script_whose_node_disappears_stops_running() {
    // 节点没了实例还留着的话，它每帧对着失效句柄空转。
    let manager = resources(&[("a.js", "return { _process() { emit('tick', 1); } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("doomed").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);
    assert_eq!(runtime.instance_count(), 1);

    scene.remove_node(node);
    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert!(signals.is_empty());
    assert_eq!(runtime.instance_count(), 0, "实例该跟着节点一起收掉");
}

// ── 即时查询：旧架构做不到的那一类 ──

#[test]
fn raycast_returns_a_result_the_script_can_act_on_immediately() {
    // 这正是「快照进、命令出」换不来的东西：当场拿到结果、当场据此行动。
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            const hit = raycast(new Vector3(0, 5, 0), Vector3.DOWN(), 100.0);
            if (hit === null) { emit('miss', 1); return; }
            emit('hit_distance', hit.distance);
            // 当场根据结果决定下一步——把自己挪到命中点上方。
            self.position = hit.position.add(Vector3.UP());
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(
        Node::new("ground")
            .with_position(Vec3::new(0.0, -0.5, 0.0))
            .with_rigid_body(RigidBody::fixed())
            .with_collider(Collider::cuboid(Vec3::new(20.0, 0.5, 20.0))),
    );
    let probe = scene.add_node(Node::new("probe").with_script("a.js"));

    scene.update();
    scene.step_physics(1.0 / 60.0);
    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);
    let hit = signals.iter().find(|s| s.name == "hit_distance");
    assert!(hit.is_some(), "射线没打中地面：{signals:?}");
    assert!((hit.unwrap().value - 5.0).abs() < 0.05);
    // 脚本当场用结果挪了自己。
    assert!((scene[probe].transform.position.y - 1.0).abs() < 0.05);
}

#[test]
fn a_raycast_that_hits_nothing_returns_null() {
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            emit('miss', raycast(new Vector3(0, 500, 0), Vector3.UP(), 10.0) === null ? 1 : 0);
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    scene.update();
    scene.step_physics(1.0 / 60.0);
    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals[0].value, 1.0);
}

// ── 物理 ──

#[test]
fn a_script_can_push_a_rigid_body() {
    let manager = resources(&[(
        "a.js",
        "return { _physics_process(dt) { self.applyImpulse(Vector3.UP().mul(10)); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let ball = scene.add_node(
        Node::new("ball")
            .with_rigid_body(RigidBody::new(
                kphysics::RigidBodyDesc::dynamic().with_gravity_scale(0.0),
            ))
            .with_collider(Collider::ball(0.5))
            .with_script("a.js"),
    );

    scene.update();
    let mut input = kinput::Input::new();
    runtime.process(&mut scene, &mut input, &manager, 0.016, 0.0);
    runtime.physics_process(&mut scene, &mut input, 1.0 / 60.0, 0.0);
    scene.step_physics(1.0 / 60.0);

    assert!(
        scene[ball].rigid_body().unwrap().linvel().y > 1.0,
        "冲量没传到刚体"
    );
}

// ── Vector3 ──

#[test]
fn vector_math_behaves() {
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            const a = new Vector3(3, 4, 0);
            emit('len', a.length());
            emit('dot', a.dot(new Vector3(1, 0, 0)));
            emit('cross', a.cross(new Vector3(0, 0, 1)).y);
            emit('norm', a.normalized().length());
            emit('lerp', new Vector3(0,0,0).lerp(a, 0.5).x);
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);
    let value = |name: &str| signals.iter().find(|s| s.name == name).unwrap().value;

    assert!((value("len") - 5.0).abs() < 1e-9);
    assert!((value("dot") - 3.0).abs() < 1e-9);
    assert!((value("cross") - (-3.0)).abs() < 1e-9);
    assert!((value("norm") - 1.0).abs() < 1e-9);
    assert!((value("lerp") - 1.5).abs() < 1e-9);
}

#[test]
fn normalising_a_zero_vector_does_not_produce_nan() {
    // 零向量归一化在数学上无定义，一路传进场景就是物体无声消失。
    let manager = resources(&[(
        "a.js",
        "return { _process() { emit('x', new Vector3(0,0,0).normalized().x); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals[0].value, 0.0);
    assert!(!signals[0].value.is_nan());
}

// ── 抗打击 ──

#[test]
fn a_nan_never_reaches_the_scene() {
    // NaN 写进变换，世界矩阵变 NaN，包围盒变 NaN，剔除判它不可见——
    // 物体无声无息地消失，日志里什么都没有。
    let manager = resources(&[(
        "a.js",
        "return { _process() {
            self.position.x = 0/0;
            self.position = new Vector3(1/0, 0, 0);
        } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert!(scene[node].transform.position.is_finite(), "NaN 漏进场景了");
    assert_eq!(scene[node].transform.position, Vec3::ZERO);
}

#[test]
fn an_infinite_loop_is_stopped_instead_of_hanging_the_engine() {
    // 没有这道闸，一个手滑的 while(true) 就能把游戏彻底冻住。
    let manager = resources(&[("a.js", "return { _process() { while (true) {} } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(runtime.stats().failed, 1, "死循环没被拦下");
}

#[test]
fn a_throwing_script_is_disabled_rather_than_spamming() {
    let manager = resources(&[(
        "a.js",
        "return { _process() { throw new Error('boom'); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);
    assert_eq!(runtime.stats().failed, 1);

    for _ in 0..3 {
        process(&mut runtime, &mut scene, &manager, 1, 0.016);
        assert_eq!(runtime.stats().failed, 0, "停用之后不该再报错");
        assert_eq!(runtime.stats().ran, 0);
    }
}

#[test]
fn one_broken_script_does_not_stop_the_others() {
    let manager = resources(&[
        ("bad.js", "return { _process() { throw 1; } };"),
        ("good.js", "return { _process() { emit('fine', 1); } };"),
    ]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("bad").with_script("bad.js"));
    scene.add_node(Node::new("good").with_script("good.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name, "fine");
}

#[test]
fn a_syntax_error_marks_the_slot_failed_and_is_not_retried() {
    let manager = resources(&[("bad.js", "return { this is not javascript };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("bad.js"));

    process(&mut runtime, &mut scene, &manager, 3, 0.016);

    assert!(scene[node].script().unwrap().failed);
    assert_eq!(runtime.instance_count(), 0);
}

#[test]
fn a_script_whose_resource_is_still_loading_waits() {
    let manager = resources(&[]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("missing.js"));

    process(&mut runtime, &mut scene, &manager, 3, 0.016);

    // 加载不出来会被标记失败，但不能 panic，也不能拖垮别的脚本。
    assert_eq!(runtime.instance_count(), 0);
    let _ = scene[node].script().unwrap();
}

#[test]
fn a_disabled_slot_never_instantiates() {
    let manager = resources(&[("a.js", "return { _process() { emit('x', 1); } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("a.js"));
    scene[node].script_mut().unwrap().enabled = false;

    let signals = process(&mut runtime, &mut scene, &manager, 2, 0.016);

    assert!(signals.is_empty());
    assert_eq!(runtime.instance_count(), 0);
}

#[test]
fn signals_carry_the_node_that_sent_them() {
    let manager = resources(&[("a.js", "return { _process() { emit('ping', 1); } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let a = scene.add_node(Node::new("a").with_script("a.js"));
    let b = scene.add_node(Node::new("b").with_script("a.js"));

    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    let sources: Vec<_> = signals.iter().map(|s| s.source).collect();
    assert!(sources.contains(&a) && sources.contains(&b), "{sources:?}");
}

#[test]
fn script_driven_motion_is_deterministic() {
    fn run() -> Vec3 {
        let manager = resources(&[(
            "a.js",
            "return { _process(dt) { self.position.x += dt * 2.0; self.rotateY(dt); } };",
        )]);
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("n").with_script("a.js"));
        process(&mut runtime, &mut scene, &manager, 30, 1.0 / 60.0);
        scene[node].transform.position
    }

    assert_eq!(run(), run());
}

// ── 热重载 ──

#[test]
fn reloading_a_script_rebuilds_its_instances() {
    // 只换资源不换实例的话，改了文件也没反应——看起来像热重载坏了。
    let manager = resources(&[("a.js", "return { _process() { emit('v1', 1); } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    let before = process(&mut runtime, &mut scene, &manager, 1, 0.016);
    assert_eq!(before[0].name, "v1");

    // 换掉磁盘上的内容（这一步平时由 kasset 的热重载做）。
    let manager = resources(&[("a.js", "return { _process() { emit('v2', 1); } };")]);
    let reset = runtime.reload_path(&mut scene, std::path::Path::new("a.js"));
    let after = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(reset, 1, "没有作废旧实例");
    assert_eq!(after[0].name, "v2", "跑的还是旧代码");
}

#[test]
fn reloading_only_touches_scripts_with_that_path() {
    let manager = resources(&[
        ("a.js", "return { _process() { emit('a', 1); } };"),
        ("b.js", "return { _process() { emit('b', 1); } };"),
    ]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let a = scene.add_node(Node::new("a").with_script("a.js"));
    let b = scene.add_node(Node::new("b").with_script("b.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);
    let reset = runtime.reload_path(&mut scene, std::path::Path::new("a.js"));

    assert_eq!(reset, 1);
    assert!(!scene[a].script().unwrap().is_live(), "a 该被作废");
    assert!(scene[b].script().unwrap().is_live(), "b 不该受牵连");
}

#[test]
fn a_backslash_path_matches_a_forward_slash_slot() {
    // Windows 上文件监视器给的是反斜杠路径，槽位里存的是正斜杠。
    // 不统一的话热重载在 Windows 上就是不响。
    let manager = resources(&[("dir/a.js", "return { _process() {} };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("dir/a.js"));
    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    let reset = runtime.reload_path(&mut scene, std::path::Path::new("dir\\a.js"));

    assert_eq!(reset, 1, "反斜杠路径没匹配上");
}

#[test]
fn reloading_gives_a_previously_broken_script_another_chance() {
    // 改错了、存盘、报错、改回来——这是热重载最常见的用法，
    // 不重置失败标记的话第二次存盘就没反应了。
    let manager = resources(&[("a.js", "return { this is broken };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    let node = scene.add_node(Node::new("n").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 1, 0.016);
    assert!(scene[node].script().unwrap().failed);

    let manager = resources(&[("a.js", "return { _process() { emit('fixed', 1); } };")]);
    runtime.reload_path(&mut scene, std::path::Path::new("a.js"));
    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name, "fixed");
}

#[test]
fn reloading_an_unrelated_path_changes_nothing() {
    let manager = resources(&[("a.js", "return { _process() { emit('a', 1); } };")]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));
    process(&mut runtime, &mut scene, &manager, 1, 0.016);

    let reset = runtime.reload_path(&mut scene, std::path::Path::new("somewhere/else.js"));

    assert_eq!(reset, 0);
    assert_eq!(runtime.instance_count(), 1);
}

#[test]
fn a_reloaded_script_starts_from_a_clean_state() {
    // 新实例的闭包变量回到初值——这一条是设计取舍，不是 bug，
    // 写成测试免得将来有人以为状态该保住。
    let manager = resources(&[(
        "a.js",
        "let n = 0; return { _process() { n += 1; emit('n', n); } };",
    )]);
    let mut runtime = ScriptRuntime::new();
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("a.js"));

    process(&mut runtime, &mut scene, &manager, 3, 0.016);
    runtime.reload_path(&mut scene, std::path::Path::new("a.js"));
    let signals = process(&mut runtime, &mut scene, &manager, 1, 0.016);

    assert_eq!(signals[0].value, 1.0, "重载后计数该从头开始");
}
