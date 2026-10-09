//! 变换与节点树：旋转、四元数、全局坐标、父子关系、路径。

use crate::{Script, ScriptRuntime};
use kasset::{MemoryResourceIo, ResourceManager};
use kmath::{EulerRot, Quat, Vec3};
use kscene::{Node, Scene};
use std::sync::Arc;

fn run(
    source: &str,
    build: impl FnOnce(&mut Scene) -> kcore::pool::Handle<Node>,
) -> (Scene, Vec<(String, f64)>) {
    let io = MemoryResourceIo::new().with("s.js", source.as_bytes().to_vec());
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    let _ = manager.request_blocking::<Script>("s.js");
    let mut scene = Scene::new();
    let _ = build(&mut scene);
    let mut runtime = ScriptRuntime::new();
    scene.update();
    let mut input = kinput::Input::new();
    let signals = runtime
        .process(&mut scene, &mut input, &manager, 1.0 / 60.0, 0.0)
        .into_iter()
        .map(|s| (s.name, s.value))
        .collect();
    assert!(runtime.errors().is_empty(), "{:?}", runtime.errors());
    (scene, signals)
}

fn get(signals: &[(String, f64)], name: &str) -> f64 {
    signals
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("没有信号 {name}：{signals:?}"))
        .1
}

#[test]
fn rotation_round_trips_in_yxz_euler() {
    let (scene, signals) = run(
        r#"return { _ready() {
            self.rotation = new Vector3(0.3, 1.2, -0.4);
            const r = self.rotation;
            emit("x", r.x); emit("y", r.y); emit("z", r.z);
            const e = self.quaternion.toEuler();
            emit("qx", e.x); emit("qy", e.y); emit("qz", e.z);
        } };"#,
        |scene| scene.add_node(Node::new("n").with_script("s.js")),
    );
    for (axis, expected) in [("x", 0.3), ("y", 1.2), ("z", -0.4)] {
        assert!((get(&signals, axis) - expected).abs() < 1e-5, "{axis}");
        // JS 侧的四元数 → 欧拉和引擎的一致。
        assert!(
            (get(&signals, &format!("q{axis}")) - expected).abs() < 1e-5,
            "q{axis}"
        );
    }
    let node = scene
        .find_by_name("n")
        .and_then(|h| scene.try_get(h))
        .unwrap();
    let expected = Quat::from_euler(EulerRot::YXZ, 1.2, 0.3, -0.4);
    assert!(node.transform.rotation.angle_between(expected) < 1e-4);
}

#[test]
fn rotate_local_versus_global() {
    // 先绕 Y 转 90°，再绕「自身的 X」和「父空间的 X」各转一次，结果的前方不同。
    let (_, signals) = run(
        r#"return { _ready() {
            self.rotateY(Math.PI / 2);
            self.rotate(new Vector3(1, 0, 0), Math.PI / 2);
            const local = self.forward;
            self.quaternion = Quaternion.identity();
            self.rotateY(Math.PI / 2);
            self.rotateGlobal(new Vector3(1, 0, 0), Math.PI / 2);
            const global = self.forward;
            emit("ly", local.y); emit("gy", global.y); emit("gz", global.z);
        } };"#,
        |scene| scene.add_node(Node::new("n").with_script("s.js")),
    );
    // 自身 X 轴此时指向世界 -Z，绕它转把前方（世界 -X）抬到了上方……
    assert!((get(&signals, "ly") - 1.0).abs() < 1e-5, "{signals:?}");
    // 绕世界 X 转则前方（-X）不动。
    assert!(
        get(&signals, "gy").abs() < 1e-5 && get(&signals, "gz").abs() < 1e-5,
        "{signals:?}"
    );
}

#[test]
fn global_position_and_local_conversion_respect_the_parent() {
    let (scene, signals) = run(
        r#"return { _ready() {
            self.globalPosition = new Vector3(10, 0, 0);
            const back = self.toGlobal(Vector3.ZERO());
            emit("gx", back.x);
            const local = self.parent.toLocal(new Vector3(10, 0, 0));
            emit("lx", local.x); emit("lz", local.z);
        } };"#,
        |scene| {
            let parent = scene.add_node(
                Node::new("parent")
                    .with_position(Vec3::new(2.0, 0.0, 0.0))
                    .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
            );
            scene.add_node_with_parent(Node::new("child").with_script("s.js"), parent)
        },
    );
    let child = scene.find_by_name("child").unwrap();
    let world = scene.world_matrix(child).w_axis.truncate();
    assert!(
        (world - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4,
        "{world}"
    );
    assert!((get(&signals, "gx") - 10.0).abs() < 1e-4);
    // 父节点转了 90°：世界 +X 方向 8 米在父空间里是 +Z 8 米。
    assert!(
        get(&signals, "lx").abs() < 1e-4 && (get(&signals, "lz") - 8.0).abs() < 1e-4,
        "{signals:?}"
    );
}

fn tree(scene: &mut Scene) -> kcore::pool::Handle<Node> {
    let level = scene.add_node(Node::new("Level"));
    let door = scene.add_node_with_parent(Node::new("Door"), level);
    scene.add_node_with_parent(Node::new("Handle"), door);
    let player = scene.add_node_with_parent(Node::new("Player").with_script("s.js"), level);
    scene.add_node_with_parent(
        Node::new("Arm").with_position(Vec3::new(0.0, 1.0, 0.0)),
        player,
    );
    player
}

#[test]
fn parent_children_and_paths() {
    let (_, signals) = run(
        r#"return { _ready() {
            emit("parentIsLevel", self.parent.name === "Level" ? 1 : 0);
            emit("children", self.children.length);
            emit("relative", self.getNode("../Door/Handle").name === "Handle" ? 1 : 0);
            emit("absolute", getNode("/Level/Door").name === "Door" ? 1 : 0);
            emit("child", self.getNode("Arm").name === "Arm" ? 1 : 0);
            emit("fallback", self.getNode("Handle").name === "Handle" ? 1 : 0);
            emit("missing", self.getNode("../Nope") === null ? 1 : 0);
            emit("deep", self.parent.findChild("Handle").name === "Handle" ? 1 : 0);
            emit("shallow", self.parent.findChild("Handle", false) === null ? 1 : 0);
            emit("rootParent", self.parent.parent === null ? 1 : 0);
            emit("equals", self.equals(getNode("Player")) ? 1 : 0);
        } };"#,
        tree,
    );
    for (name, value) in &signals {
        if name == "children" {
            assert_eq!(*value, 1.0);
        } else {
            assert_eq!(*value, 1.0, "{name}");
        }
    }
    assert_eq!(signals.len(), 11);
}

#[test]
fn reparent_keeps_the_global_position_by_default() {
    let (scene, signals) = run(
        r#"return { _ready() {
            const arm = self.getNode("Arm");
            emit("ok", arm.reparent(getNode("Door")) ? 1 : 0);
            emit("cycle", self.parent.reparent(arm) ? 1 : 0);
        } };"#,
        |scene| {
            let player = tree(scene);
            if let Some(node) = scene.try_get_mut(player) {
                node.transform.position = Vec3::new(5.0, 0.0, 0.0);
            }
            player
        },
    );
    assert_eq!(get(&signals, "ok"), 1.0);
    assert_eq!(get(&signals, "cycle"), 0.0, "不能挂到自己的子孙下面");
    let arm = scene.find_by_name("Arm").unwrap();
    let door = scene.find_by_name("Door").unwrap();
    assert_eq!(scene.try_get(arm).unwrap().parent(), door);
    let world = scene.world_matrix(arm).w_axis.truncate();
    assert!(
        (world - Vec3::new(5.0, 1.0, 0.0)).length() < 1e-4,
        "{world}"
    );
}

#[test]
fn quaternion_math_matches_the_engine() {
    let (_, signals) = run(
        r#"return { _ready() {
            const q = Quaternion.fromAxisAngle(Vector3.UP(), Math.PI / 2);
            const v = q.rotate(new Vector3(1, 0, 0));
            emit("vx", v.x); emit("vz", v.z);
            const half = Quaternion.identity().slerp(q, 0.5);
            emit("half", half.angleTo(Quaternion.identity()));
            const ft = Quaternion.fromTo(new Vector3(0, 0, -1), new Vector3(1, 0, 0)).rotate(new Vector3(0, 0, -1));
            emit("ftx", ft.x);
            self.quaternion = q;
            const f = self.forward;
            emit("fx", f.x);
        } };"#,
        |scene| scene.add_node(Node::new("n").with_script("s.js")),
    );
    assert!(get(&signals, "vx").abs() < 1e-9 && (get(&signals, "vz") + 1.0).abs() < 1e-9);
    assert!((get(&signals, "half") - std::f64::consts::FRAC_PI_4).abs() < 1e-6);
    assert!((get(&signals, "ftx") - 1.0).abs() < 1e-9);
    // 绕 Y 转 90°：前方 -Z 转到 -X。
    assert!((get(&signals, "fx") + 1.0).abs() < 1e-5);
}

#[test]
fn bad_values_are_ignored_not_written() {
    let (scene, _) = run(
        r#"return { _ready() {
            self.rotation = new Vector3(NaN, 0, 0);
            self.quaternion = new Quaternion(0, 0, 0, 0);
            self.globalPosition = new Vector3(Infinity, 0, 0);
        } };"#,
        |scene| scene.add_node(Node::new("n").with_script("s.js")),
    );
    let node = scene
        .find_by_name("n")
        .and_then(|h| scene.try_get(h))
        .unwrap();
    assert_eq!(node.transform.rotation, Quat::IDENTITY);
    assert_eq!(node.transform.position, Vec3::ZERO);
}
