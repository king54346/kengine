//! 能不能吃下 bevy 那批标准测试模型。
//!
//! `examples/kengine/` 下的移植例子都用 `bevy-main/assets/` 里的模型——
//! 用同一份资源，两边的画面才好对着看。这里先把「加载得进来」这件事钉住：
//! 例子跑不出东西时，第一个要排除的就是模型压根没读进来。
//!
//! 这些是**外部引用**形式的 glTF（`.gltf` + 独立 `.bin` + 一堆 `.png`），
//! 和仓库里自带的 `.glb`（一个文件打包全部）走的不是同一条路径。

use kengine::prelude::*;

/// bevy 的资源目录。它是个 git 子目录，不是 kengine 自己的资源。
const BEVY: &str = "bevy-main/assets/models";

fn manager() -> ResourceManager {
    let manager = ResourceManager::new();
    manager.add_loader(GltfLoader);
    manager.add_loader(TextureLoader);
    manager
}

fn load(path: &str) -> Resource<Model> {
    manager()
        .request_blocking::<Model>(path)
        .unwrap_or_else(|error| panic!("{path} 读不进来：{error}"))
}

#[test]
fn flight_helmet_loads_with_external_buffers_and_textures() {
    let model = load(&format!("{BEVY}/FlightHelmet/FlightHelmet.gltf"));
    let data = model.data_ref().expect("解码失败");

    // 这个模型是六个部件分开的，正好用来试「一个场景多个网格 + 多份材质」。
    assert!(data.nodes().len() >= 6, "节点太少：{}", data.nodes().len());
    assert!(
        data.meshes().len() >= 6,
        "网格太少：{}",
        data.meshes().len()
    );
    assert!(
        data.materials().len() >= 4,
        "材质太少：{}",
        data.materials().len()
    );
    assert!(data.triangle_count() > 10_000);
}

#[test]
fn gltf_primitives_loads() {
    // 一个 mesh 里带多个 primitive（各自一份材质）——bevy 那边
    // `query_gltf_primitives` 就是拿它演示「按材质名找到其中一块」的。
    let model = load(&format!("{BEVY}/GltfPrimitives/gltf_primitives.glb"));
    let data = model.data_ref().expect("解码失败");

    assert!(data.meshes().len() >= 2);
    assert!(data.materials().len() >= 2);
}

#[test]
fn simple_skin_loads_with_joints() {
    let model = load(&format!("{BEVY}/SimpleSkin/SimpleSkin.gltf"));
    let data = model.data_ref().expect("解码失败");

    let skin = data.skin(0).expect("没有骨架");
    assert_eq!(skin.len(), 2, "SimpleSkin 是两根骨头");
}

#[test]
fn material_names_survive_the_import() {
    // 按名字找材质是 `edit_material_on_gltf` / `query_gltf_primitives` 的
    // 全部前提。名字丢了的话，只能靠「第几个 primitive」去猜，
    // 而那个序号美术重新导出一次就变了。
    let model = load(&format!("{BEVY}/FlightHelmet/FlightHelmet.gltf"));
    let data = model.data_ref().expect("解码失败");

    let names: Vec<&str> = data.materials().iter().filter_map(|m| m.name()).collect();

    assert!(
        names.contains(&"LeatherPartsMat"),
        "没读到 glTF 里的材质名，只有：{names:?}"
    );
}

#[test]
fn gltf_extras_are_kept_verbatim() {
    // extras 是 glTF 留给各家塞自定义数据的口袋：Blender 的自定义属性、
    // 关卡编辑器的标记都走它。规范不约定结构，所以引擎只负责原样留下。
    let model = load(&format!("{BEVY}/extras/gltf_extras.glb"));
    let data = model.data_ref().expect("解码失败");
    let extras = data.extras();

    assert!(!extras.is_empty(), "一条 extras 都没读到");

    // 这个文件里四种 extras 各有一条，它就是为此造出来的。
    assert!(extras.scene.is_some(), "场景级 extras 丢了");
    assert!(extras.nodes.iter().any(Option::is_some), "节点 extras 丢了");
    assert!(
        extras.meshes.iter().any(Option::is_some),
        "网格 extras 丢了"
    );
    assert!(
        extras.materials.iter().any(Option::is_some),
        "材质 extras 丢了"
    );

    // 存的是原文而不是解析结果——里面有什么由游戏自己决定。
    let node = extras.nodes.iter().flatten().next().expect("节点 extras");
    assert!(node.trim_start().starts_with('{'), "不像 JSON：{node}");
}
