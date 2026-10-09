//! 新增格式在 three.js 真实样本上的冒烟测试。
//!
//! 单元测试里手工拼的小文件只能证明解析器「自洽」；真实导出器写出来的
//! 文件才会暴露大小写、可选字段、奇怪的嵌套。样本不在时跳过。

use kasset::{Resource, ResourceData, ResourceManager};
use kgltf::Model;
use std::path::PathBuf;

fn sample(relative: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/threejs/models")
        .join(relative);
    path.exists().then_some(path)
}

fn load<T: ResourceData>(
    relative: &str,
    loader: impl kasset::ResourceLoader + 'static,
) -> Option<Resource<T>> {
    let path = sample(relative)?;
    let manager = ResourceManager::new();
    manager.add_loader(loader);
    Some(
        manager
            .request_blocking::<T>(path)
            .unwrap_or_else(|e| panic!("{relative} 导入失败：{e}")),
    )
}

fn assert_model(model: &Model, relative: &str) {
    assert!(model.triangle_count() > 0, "{relative} 没有三角形");
    for mesh in model.meshes() {
        for v in mesh.vertices() {
            assert!(v.position().is_finite(), "{relative} 有非有限顶点");
        }
    }
}

#[test]
fn amf_rook() {
    let Some(model) = load::<Model>("amf/rook.amf", kimport::AmfLoader) else {
        return;
    };
    assert_model(&model.data_ref().unwrap(), "rook.amf");
}

#[test]
fn threemf_samples() {
    for name in [
        "cube_gears",
        "facecolors",
        "multipletextures",
        "truck",
        "vertexcolors",
        "volumetric",
    ] {
        let relative = format!("3mf/{name}.3mf");
        let Some(model) = load::<Model>(&relative, kimport::ThreeMfLoader) else {
            continue;
        };
        let model = model.data_ref().unwrap();
        assert_model(&model, &relative);
        if name == "truck" || name == "multipletextures" {
            assert!(
                model
                    .materials()
                    .iter()
                    .any(|m| m.base_color_texture().is_some()),
                "{relative} 的贴图没挂上"
            );
        }
    }
}

#[test]
fn tds_portalgun() {
    let Some(model) = load::<Model>("3ds/portalgun/portalgun.3ds", kimport::TdsLoader) else {
        return;
    };
    let model = model.data_ref().unwrap();
    assert_model(&model, "portalgun.3ds");
}

#[test]
fn drc_bunny() {
    let Some(model) = load::<Model>("draco/bunny.drc", kimport::DracoLoader) else {
        return;
    };
    let model = model.data_ref().unwrap();
    assert_model(&model, "bunny.drc");
    assert!(model.triangle_count() > 1000);
}

#[test]
fn bvh_pirouette() {
    let Some(model) = load::<Model>("bvh/pirouette.bvh", kimport::BvhLoader) else {
        return;
    };
    let model = model.data_ref().unwrap();
    assert!(model.nodes().len() > 20);
    assert!(model.animations()[0].duration() > 1.0);
}

#[test]
fn gcode_benchy() {
    let Some(code) = load::<kimport::gcode::GCode>("gcode/benchy.gcode", kimport::GCodeLoader)
    else {
        return;
    };
    let code = code.data_ref().unwrap();
    assert!(code.layers.len() > 50, "只有 {} 层", code.layers.len());
    assert!(code.extrusion_count() > 10_000);
}

#[test]
fn collada_samples() {
    for relative in [
        "collada/elf/elf.dae",
        "collada/stormtrooper/stormtrooper.dae",
        "collada/abb_irb52_7_120.dae",
        "collada/pump/pump.dae",
    ] {
        let Some(model) = load::<Model>(relative, kimport::ColladaLoader) else {
            continue;
        };
        let model = model.data_ref().unwrap();
        assert_model(&model, relative);
        if relative.contains("elf") {
            assert!(
                model
                    .materials()
                    .iter()
                    .filter(|m| m.base_color_texture().is_some())
                    .count()
                    >= 3,
                "elf 的贴图没挂全"
            );
        }
        if relative.contains("stormtrooper") {
            assert_eq!(model.skins().len(), 1);
            assert!(model.skins()[0].joints.len() > 30);
            assert!(
                model.meshes().iter().any(|m| m.skin().is_some()),
                "蒙皮权重没写进网格"
            );
            assert!(model.animations()[0].duration() > 1.0);
        }
    }
}

#[test]
fn collada_kinematics() {
    let Some(collada) = load::<kimport::collada::Collada>(
        "collada/abb_irb52_7_120.dae",
        kimport::ColladaKinematicsLoader,
    ) else {
        return;
    };
    let collada = collada.data_ref().unwrap();
    let movable: Vec<_> = collada.joints.iter().filter(|j| !j.is_static()).collect();
    assert_eq!(
        movable.len(),
        6,
        "ABB 机械臂应当有 6 个可动关节：{:?}",
        collada.joints.iter().map(|j| &j.name).collect::<Vec<_>>()
    );
    // 关节 1 绕 Z 转 90°：节点的局部旋转应当正好是这个。
    let joint = movable.iter().find(|j| j.name == "joint_1").unwrap();
    let t = joint.transform(90.0);
    assert!(
        t.rotation
            .dot(kmath::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2))
            .abs()
            > 0.9999
    );
}

#[test]
fn kmz_box() {
    let Some(model) = load::<Model>("kmz/Box.kmz", kimport::KmzLoader) else {
        return;
    };
    assert_model(&model.data_ref().unwrap(), "Box.kmz");
}

#[test]
fn fbx_samples() {
    for name in [
        "Samba Dancing",
        "morph_test",
        "monkey",
        "monkey_embedded_texture",
        "vCube",
        "stanford-bunny",
        "mixamo",
        "RotationTest",
        "exampleWindow",
        "morph-translation",
        "archer/ArcherRi01",
        "warrior/Warrior",
        "Head_69",
    ] {
        let relative = format!("fbx/{name}.fbx");
        let Some(model) = load::<Model>(&relative, kimport::FbxLoader) else {
            continue;
        };
        let model = model.data_ref().unwrap();
        assert_model(&model, &relative);
        match name {
            "Samba Dancing" | "mixamo" => {
                assert!(!model.skins().is_empty(), "{relative} 没有骨架");
                assert!(
                    model.meshes().iter().any(|m| m.skin().is_some()),
                    "{relative} 没有蒙皮权重"
                );
                assert!(
                    model
                        .animations()
                        .first()
                        .is_some_and(|c| c.duration() > 1.0),
                    "{relative} 没有动画"
                );
            }
            "morph_test" => assert!(
                model.meshes().iter().any(|m| !m.morph_targets().is_empty()),
                "形变目标没读出来"
            ),
            "monkey" | "monkey_embedded_texture" => {
                assert!(
                    model
                        .materials()
                        .iter()
                        .any(|m| m.base_color_texture().is_some()),
                    "{relative} 的贴图没挂上"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn fbx_nurbs() {
    let Some(fbx) = load::<kimport::fbx::Fbx>("fbx/nurbs.fbx", kimport::FbxSceneLoader) else {
        return;
    };
    let fbx = fbx.data_ref().unwrap();
    assert!(fbx.curves.len() >= 3, "只有 {} 条曲线", fbx.curves.len());
    for (_, points) in &fbx.curves {
        assert!(
            points
                .iter()
                .all(|p| p.is_finite() && p.abs().max_element() < 100.0)
        );
    }
}

/// 43 MB 的 Revit 样本。调试构建下解析要一分钟以上，所以默认忽略：
/// `cargo test -p kimport --release --test new_formats -- --ignored ifc`
#[test]
#[ignore]
fn ifc_revit_sample() {
    let started = std::time::Instant::now();
    let Some(model) = load::<Model>("ifc/rac_advanced_sample_project.ifc", kimport::IfcLoader)
    else {
        return;
    };
    let model = model.data_ref().unwrap();
    assert_model(&model, "rac_advanced_sample_project.ifc");
    assert!(
        model.triangle_count() > 100_000,
        "只有 {} 个三角形",
        model.triangle_count()
    );
    assert!(
        model.meshes().len() < 200,
        "按颜色合并后应当只剩几十块网格，实际 {}",
        model.meshes().len()
    );
    eprintln!(
        "IFC：{} 个三角形 / {} 块网格，{:.1}s",
        model.triangle_count(),
        model.meshes().len(),
        started.elapsed().as_secs_f32()
    );
}

#[test]
fn rhino_logo() {
    let Some(scene) =
        load::<kimport::threedm::Rhino3dm>("3dm/Rhino_Logo.3dm", kimport::Rhino3dmSceneLoader)
    else {
        return;
    };
    let scene = scene.data_ref().unwrap();
    eprintln!(
        "3DM：{} 个图层 {:?}，{} 块网格 / {} 个三角形，{} 条曲线，{} 个点",
        scene.layers.len(),
        scene.layers.iter().map(|l| &l.name).collect::<Vec<_>>(),
        scene.model.meshes().len(),
        scene.model.triangle_count(),
        scene.curves.len(),
        scene.points.len()
    );
    for (i, l) in scene.layers.iter().enumerate() {
        eprintln!(
            "  图层 {} 下 {} 个网格物体",
            l.name,
            scene.model.nodes()[1 + i].children.len()
        );
    }
    assert!(scene.layers.len() > 1);
    assert!(scene.model.triangle_count() > 0);
    assert!(scene.curves.len() > 100);
    for curve in &scene.curves {
        assert!(curve.points.iter().all(|p| p.is_finite()));
    }
}

/// `assets/tiles/`（由 `gen_3dtiles.py` 生成）：瓦片集解析、b3dm 内容。
#[test]
fn local_tileset() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/tiles");
    if !root.join("tileset.json").exists() {
        return;
    }
    let manager = ResourceManager::new();
    manager.add_loader(kimport::TilesetLoader);
    manager.add_loader(kimport::B3dmLoader);
    let tileset = manager
        .request_blocking::<kimport::tiles::Tileset>(root.join("tileset.json"))
        .unwrap();
    let tileset = tileset.data_ref().unwrap();
    assert_eq!(tileset.tiles.len(), 85);
    let b3dm = tileset.tiles[0].content.clone().unwrap();
    let model = manager.request_blocking::<Model>(b3dm).unwrap();
    assert_model(&model.data_ref().unwrap(), "0_0_0.b3dm");
}
