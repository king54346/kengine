//! 场景图的两条热路径：每帧的 `update` 与视锥剔除。
//!
//! 这两条是**每帧必走**的，而且随节点数线性增长，是最容易悄悄退化的地方。
//! 之前判断它们快不快靠的是手工跑 demo 读日志，那台机器波动能到 ±40%，
//! 相邻两次的数字差一倍都说明不了任何事。
//!
//! 用 criterion 而不是自己掐表：它做的两件事恰好是手工计时做不到的——
//! 跑足够多次直到置信区间收窄，以及把上一次的结果存下来做对比。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kengine::prelude::*;
use std::hint::black_box;

/// 各档规模。跨一个数量级，好看出是线性还是更糟。
const SIZES: [usize; 4] = [100, 1_000, 5_000, 20_000];

/// 铺一片方块。`depth` 控制层级深度：变换传播是沿父链走的，
/// 一棵深树和一片平铺的开销完全不是一回事。
fn flat_scene(count: usize) -> Scene {
    let mut scene = Scene::new();
    let mesh = Mesh::cube();
    let side = (count as f32).cbrt().ceil() as usize;

    for i in 0..count {
        let (x, y, z) = (i % side, (i / side) % side, i / (side * side));
        scene.add_node(
            // 网格是克隆的：克隆共享同一份 id，这里量的是场景图本身的开销，
            // 不是几万份顶点数据的分配。
            Node::new(format!("n{i}"))
                .with_mesh(mesh.clone())
                .with_position(Vec3::new(x as f32 * 2.0, y as f32 * 2.0, z as f32 * 2.0)),
        );
    }
    scene.update();
    scene
}

/// 一条深链。每个节点都是上一个的子节点。
fn deep_scene(count: usize) -> Scene {
    let mut scene = Scene::new();
    let mesh = Mesh::cube();
    let mut parent = scene.root();

    for i in 0..count {
        parent = scene.add_node_with_parent(
            Node::new(format!("n{i}"))
                .with_mesh(mesh.clone())
                .with_position(Vec3::new(0.1, 0.1, 0.1)),
            parent,
        );
    }
    scene.update();
    scene
}

fn scene_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("scene_update");

    for size in SIZES {
        // 平铺：`update` 的主要成本是重算包围盒与重建剔除结构。
        group.bench_with_input(BenchmarkId::new("flat", size), &size, |b, &size| {
            let mut scene = flat_scene(size);
            b.iter(|| {
                scene.update();
                black_box(scene.drawable_count());
            });
        });
    }

    // 深链单独一档：这条路径考验的是变换传播，不是包围盒。
    // 不跑到 20 000——一条两万节点的链在真实场景里不存在，
    // 跑它只是在量一个不会发生的情况。
    for size in [100, 1_000] {
        group.bench_with_input(BenchmarkId::new("deep", size), &size, |b, &size| {
            let mut scene = deep_scene(size);
            b.iter(|| {
                scene.update();
                black_box(scene.drawable_count());
            });
        });
    }

    group.finish();
}

fn culling(c: &mut Criterion) {
    let mut group = c.benchmark_group("culling");

    let projection = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 200.0);

    for size in SIZES {
        let scene = flat_scene(size);

        // 相机看向场景中心：一部分在视锥内、一部分在外。
        // 全在内或全在外都会让 BVH 走上快路径，量出来的数字偏乐观。
        let view = Mat4::look_at_rh(Vec3::splat(-40.0), Vec3::splat(20.0), Vec3::Y);
        let frustum = Frustum::from_view_projection(projection * view);

        group.bench_with_input(BenchmarkId::new("partial", size), &size, |b, _| {
            b.iter(|| black_box(scene.cull(black_box(&frustum))).len());
        });
    }

    // 全部可见：量的是「剔除结构不帮忙时」的下界，也就是收集与建项的成本。
    let scene = flat_scene(5_000);
    let view = Mat4::look_at_rh(Vec3::splat(-500.0), Vec3::ZERO, Vec3::Y);
    let wide = Mat4::perspective_rh(2.8, 1.0, 0.1, 5_000.0);
    let frustum = Frustum::from_view_projection(wide * view);
    group.bench_function("all_visible/5000", |b| {
        b.iter(|| black_box(scene.cull(black_box(&frustum))).len());
    });

    group.finish();
}

criterion_group!(benches, scene_update, culling);
criterion_main!(benches);
