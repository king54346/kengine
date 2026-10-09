//! 蒙皮的绑定姿态：静止时每个关节的「世界矩阵 × 逆绑定」必须是单位阵，
//! 否则一播动画（甚至不播）人物就炸开。
//!
//! Mixamo 导出的 FBX 把 Cluster 的 `Transform` 写成了 `TransformLink` 的逆，
//! 照字面意思当「网格的绑定变换」用，绑定姿态会平移两倍——这个测试钉住它。

use kmath::Mat4;
use std::path::Path;

fn check(file: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/threejs/models/fbx")
        .join(file);
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("没有 {}，跳过", path.display());
        return;
    };
    let fbx = ktask::block_on(kimport::fbx::parse_fbx(
        bytes,
        path.clone(),
        std::sync::Arc::new(kasset::FsResourceIo),
    ))
    .unwrap_or_else(|e| panic!("{file}：{e}"));
    let model = &fbx.model;
    let nodes = model.nodes();
    let mut parent = vec![None; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for &child in &node.children {
            parent[child] = Some(i);
        }
    }
    let world = |mut i: usize| {
        let mut m = nodes[i].transform.matrix();
        while let Some(p) = parent[i] {
            m = nodes[p].transform.matrix() * m;
            i = p;
        }
        m
    };
    assert!(!model.skins().is_empty(), "{file} 应该有蒙皮");
    for skin in model.skins() {
        for (&joint, inverse) in skin.joints.iter().zip(&skin.inverse_bind) {
            // 网格节点自己也可能有变换：比的是「关节相对网格」。
            let error = (world(joint) * *inverse - Mat4::IDENTITY)
                .to_cols_array()
                .iter()
                .map(|v| v.abs())
                .fold(0.0f32, f32::max);
            // 根节点的上轴旋转同时作用于关节和网格，比较时要把网格那份去掉——
            // 样本的网格都挂在根下，世界矩阵就是根的旋转。
            let mesh = world(0);
            let relative = (mesh.inverse() * world(joint) * *inverse - Mat4::IDENTITY)
                .to_cols_array()
                .iter()
                .map(|v| v.abs())
                .fold(0.0f32, f32::max);
            assert!(
                error.min(relative) < 1e-2,
                "{file} 关节 {} 的绑定姿态偏了 {error}",
                nodes[joint].name
            );
        }
    }
}

#[test]
fn mixamo_bind_poses_line_up_with_the_rest_pose() {
    check("Samba Dancing.fbx");
    check("mixamo.fbx");
}
