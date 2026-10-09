//! 用 three.js 仓库里的真实样本验证压缩扩展的解码。
//!
//! 手工构造的字节流只能证明「自洽」，证明不了「和编码器一致」——
//! meshopt / Draco 的解码器写错一位，自洽的测试照样通过，而真实文件
//! 会解出一团乱麻。所以这里直接读 `examples/threejs` 下的文件；
//! 样本不在（比如精简过的仓库）时跳过而不是失败。

use kasset::ResourceManager;
use kgltf::{GltfLoader, Model};
use std::path::PathBuf;

fn sample(relative: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/threejs/models/gltf")
        .join(relative);
    path.exists().then_some(path)
}

fn load(path: PathBuf) -> kasset::Resource<Model> {
    let manager = ResourceManager::new();
    manager.add_loader(GltfLoader);
    manager
        .request_blocking::<Model>(path)
        .expect("样本应当能导入")
}

/// 所有顶点都是有限数，且落在一个不离谱的包围盒里。
///
/// 解码错了的网格最常见的样子是几个顶点飞到 1e30——检查这个比比对
/// 具体数值更能说明问题，也不依赖参考实现的输出。
fn assert_sane(model: &Model, extent: f32) {
    assert!(model.triangle_count() > 0);
    for mesh in model.meshes() {
        for vertex in mesh.vertices() {
            let p = vertex.position();
            assert!(
                p.is_finite() && p.abs().max_element() < extent,
                "顶点 {p:?} 不合理"
            );
            let n = kmath::Vec3::from_array(vertex.normal);
            assert!((n.length() - 1.0).abs() < 0.05, "法线 {n:?} 没有归一化");
        }
        let count = mesh.vertices().len() as u32;
        assert!(mesh.indices().iter().all(|&i| i < count));
    }
}

#[test]
fn meshopt_quantized_basisu_coffeemat() {
    let Some(path) = sample("coffeemat.glb") else {
        return;
    };
    let model = load(path);
    let model = model.data_ref().unwrap();
    // 量化位置是非归一化的 u16，缩放放在节点变换里，所以网格空间的
    // 坐标本来就在 0..65535。
    assert_sane(&model, 65536.0);
    // 五张 KHR_texture_basisu 贴图至少要有一张真的解出来挂到材质上。
    let textured = model
        .materials()
        .iter()
        .filter(|m| m.base_color_texture().is_some())
        .count();
    assert!(textured > 0, "Basis 贴图一张都没挂上");
}

#[test]
fn draco_avif_forest_house() {
    let Some(path) = sample("AVIFTest/forest_house.glb") else {
        return;
    };
    let model = load(path);
    let model = model.data_ref().unwrap();
    assert_sane(&model, 1000.0);
}

/// `assets/animation_pointer.gltf`（由 `gen_animation_pointer.py` 生成）：
/// 指针通道要被摘出来、翻译成轨道，而不是让整个文件读不出来。
#[test]
fn animation_pointer_tracks_are_translated() {
    use kanim::{Channel, MaterialProperty};
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/animation_pointer.gltf");
    if !path.exists() {
        return;
    }
    let model = load(path);
    let model = model.data_ref().unwrap();
    let clip = &model.animations()[0];
    let has = |f: &dyn Fn(&Channel) -> bool| clip.tracks().iter().any(|t| f(&t.channel));
    assert!(
        has(&|c| matches!(c, Channel::Rotation(_))),
        "节点旋转指针没翻译"
    );
    assert!(has(&|c| matches!(
        c,
        Channel::Property {
            property: MaterialProperty::BaseColor,
            ..
        }
    )));
    assert!(has(&|c| matches!(
        c,
        Channel::Property {
            property: MaterialProperty::Emissive,
            ..
        }
    )));
    assert!(has(&|c| matches!(
        c,
        Channel::Property {
            property: MaterialProperty::Param(1, 0),
            ..
        }
    )));

    // 采样半圈：颜色循环应当走到了别的颜色，强度应当接近峰值 4.2。
    let pose = clip.sample(2.0);
    let emissive = pose
        .properties()
        .iter()
        .find(|p| p.property == MaterialProperty::Emissive)
        .unwrap()
        .value;
    assert!((emissive.x - 4.2).abs() < 0.05, "自发光 {emissive:?}");
    // 虹彩材质的静态系数是 0，但因为被指针驱动，必须换上扩展着色器。
    assert!(
        model.materials()[3].shader().is_some(),
        "虹彩材质没换上扩展着色器"
    );
}
