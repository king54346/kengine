//! CPU 侧每帧要做的准备工作：粒子推进、蒙皮、调试线生成。
//!
//! 这三条的共同点是**不碰 GPU**，所以能在没有显卡的机器上（包括 CI）跑。
//! 真正的批处理与绘制提交藏在 `Renderer::render` 里，那需要一个 wgpu 设备，
//! headless 量不了——那部分只能继续靠 demo 里的 `prepare_micros`。
//! 这是这套 benches 已知的盲区，不假装覆盖了。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kengine::prelude::*;
use std::hint::black_box;

fn particles(c: &mut Criterion) {
    let mut group = c.benchmark_group("particles");

    for capacity in [1_000usize, 10_000, 50_000] {
        // 先让粒子池填满：空池的 tick 只是在走一遍空循环，
        // 量出来的数字跟实际运行时毫无关系。
        let warm = |capacity: usize| {
            let mut system = ParticleSystem::new(
                Emitter::default()
                    .with_rate(capacity as f32 * 10.0)
                    .with_lifetime((5.0, 10.0))
                    .with_speed((1.0, 4.0)),
            )
            .with_capacity(capacity);
            for _ in 0..30 {
                system.tick(1.0 / 60.0, Mat4::IDENTITY);
            }
            system
        };

        group.bench_with_input(
            BenchmarkId::new("tick", capacity),
            &capacity,
            |b, &capacity| {
                let mut system = warm(capacity);
                b.iter(|| {
                    system.tick(black_box(1.0 / 60.0), black_box(Mat4::IDENTITY));
                    black_box(system.alive())
                });
            },
        );

        // 收集是另一条路径：它按到相机的距离排序后打包成 GPU 结构体，
        // 排序是 O(n log n)，和 tick 的 O(n) 不是一回事。
        group.bench_with_input(
            BenchmarkId::new("collect", capacity),
            &capacity,
            |b, &capacity| {
                let system = warm(capacity);
                let mut out = Vec::with_capacity(capacity);
                b.iter(|| {
                    out.clear();
                    system.collect(Mat4::IDENTITY, black_box(Vec3::splat(20.0)), &mut out);
                    black_box(out.len())
                });
            },
        );
    }

    group.finish();
}

fn skinning(c: &mut Criterion) {
    let mut group = c.benchmark_group("skinning");

    // 一条 N 关节的骨架，蒙一个网格。`Scene::update` 里骨骼矩阵是
    // 「关节世界变换 × 逆绑定矩阵」，随关节数线性增长。
    for joints in [32usize, 128, 512] {
        group.bench_with_input(BenchmarkId::new("update", joints), &joints, |b, &joints| {
            let mut scene = Scene::new();
            let mut chain = Vec::with_capacity(joints);
            let mut parent = scene.root();
            for i in 0..joints {
                parent = scene.add_node_with_parent(
                    Node::new(format!("j{i}")).with_position(Vec3::Y * 0.1),
                    parent,
                );
                chain.push(parent);
            }
            scene.add_node(
                Node::new("skinned")
                    .with_mesh(Mesh::cube())
                    .with_skin(Skin::new(chain, vec![Mat4::IDENTITY; joints])),
            );
            scene.update();

            b.iter(|| {
                scene.update();
                black_box(scene.drawable_count())
            });
        });
    }

    group.finish();
}

fn gizmos(c: &mut Criterion) {
    let mut group = c.benchmark_group("gizmos");

    // 形状生成的吞吐。写这套 benches 的直接动因之一就是这条：
    // 在 demo 里开满调试叠加层与关闭相比，CPU 准备耗时的区间**完全重叠**
    // （763~1539 µs vs 745~1706 µs），机器波动盖过了真实差异。
    group.bench_function("sphere/1000", |b| {
        let mut g = Gizmos::new();
        g.set_enabled(true);
        b.iter(|| {
            g.clear();
            for i in 0..1_000 {
                g.sphere(Vec3::splat(i as f32), 1.0, GizmoColor::CYAN);
            }
            black_box(g.len())
        });
    });

    group.bench_function("aabb/10000", |b| {
        let mut g = Gizmos::new();
        g.set_enabled(true);
        let boxes: Vec<Aabb> = (0..10_000)
            .map(|i| Aabb::from_center_half_extents(Vec3::splat(i as f32), Vec3::ONE))
            .collect();
        b.iter(|| {
            g.clear();
            for aabb in &boxes {
                g.aabb(*aabb, GizmoColor::GREEN);
            }
            black_box(g.len())
        });
    });

    // 关掉时的成本。宣称的是「关闭时连形状都不算」，这条就是那句话的度量：
    // 它应当比开着时快到不在同一个量级上。
    group.bench_function("disabled/1000_spheres", |b| {
        let mut g = Gizmos::new();
        b.iter(|| {
            for i in 0..1_000 {
                g.sphere(Vec3::splat(i as f32), 1.0, GizmoColor::CYAN);
            }
            black_box(g.len())
        });
    });

    // 整套内置叠加层：这是按下 H 键之后每帧真正付出的代价。
    group.bench_function("scene_overlays/1000_nodes", |b| {
        let mut scene = Scene::new();
        let mesh = Mesh::cube();
        for i in 0..1_000 {
            scene.add_node(
                Node::new(format!("n{i}"))
                    .with_mesh(mesh.clone())
                    .with_position(Vec3::new((i % 32) as f32 * 2.0, 0.0, (i / 32) as f32 * 2.0)),
            );
        }
        scene.update();
        scene.gizmos_mut().set_enabled(true);

        let options = SceneDebugOptions {
            bounds: true,
            bvh: true,
            node_axes: true,
            skeletons: false,
            lights: true,
            cameras: true,
        };
        b.iter(|| {
            scene.gizmos_mut().clear();
            scene.debug_draw(black_box(options));
            black_box(scene.gizmos().len())
        });
    });

    group.finish();
}

criterion_group!(benches, particles, skinning, gizmos);
criterion_main!(benches);
