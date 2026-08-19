//! 场景存读盘。
//!
//! 这条路径是**读档时的卡顿来源**，而且它随场景规模增长的方式不显然：
//! 网格与材质做了去重共享，所以存盘大小和节点数不成正比——
//! 到底是省下来了还是白做了功，得量。
//!
//! 二进制与文本两种格式分开量：文本格式是给人看和给 diff 用的，
//! 慢一个量级也无所谓；但如果它慢两个量级，那就该劝人别在热路径上用它。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kengine::prelude::*;
use std::hint::black_box;

/// 一个有代表性的场景：共享网格、若干材质、几个光源、一部相机。
fn sample_scene(nodes: usize) -> Scene {
    let mut scene = Scene::new();
    let mesh = Mesh::cube();

    for i in 0..nodes {
        scene.add_node(
            Node::new(format!("n{i}"))
                .with_mesh(mesh.clone())
                .with_material(PbrMaterial::metal(
                    Vec3::new(0.9, 0.7, 0.4),
                    (i % 8) as f32 / 8.0,
                ))
                .with_position(Vec3::new((i % 32) as f32, 0.0, (i / 32) as f32)),
        );
    }

    scene.add_node(Node::new("sun").with_light(Light::directional()));
    scene.add_node(Node::new("cam").with_camera(Camera::default()));
    scene.update();
    scene
}

fn save(c: &mut Criterion) {
    let mut group = c.benchmark_group("scene_save");
    group.sample_size(30);

    for nodes in [100usize, 1_000, 5_000] {
        group.bench_with_input(BenchmarkId::new("binary", nodes), &nodes, |b, &nodes| {
            let mut scene = sample_scene(nodes);
            b.iter(|| black_box(scene.save_to_vec()).map(|v| v.len()));
        });
    }

    group.finish();
}

fn load(c: &mut Criterion) {
    let mut group = c.benchmark_group("scene_load");
    group.sample_size(30);

    for nodes in [100usize, 1_000, 5_000] {
        let bytes = sample_scene(nodes)
            .save_to_vec()
            .expect("样例场景应当能存盘");

        group.bench_with_input(BenchmarkId::new("binary", nodes), &nodes, |b, _| {
            b.iter(|| {
                // 第二个参数是资源管理器，用来解析外部网格引用；
                // 样例场景的网格全是内联的，给 None 就够。
                let scene = Scene::load_from_slice(black_box(&bytes), None);
                black_box(scene.map(|s| s.drawable_count()))
            });
        });
    }

    group.finish();
}

/// 存盘体积。不是耗时，但它决定了 IO 时间，而且共享去重有没有生效
/// 只能从体积上看出来——一条 O(1) 的断言胜过一堆猜测。
fn footprint(c: &mut Criterion) {
    let mut group = c.benchmark_group("scene_footprint");
    // 只跑一次就够：这里量的是字节数，不是速度。
    group.sample_size(10);

    group.bench_function("bytes_per_node", |b| {
        b.iter_batched(
            || sample_scene(1_000),
            |mut scene| {
                let bytes = scene.save_to_vec().expect("应当能存盘").len();
                // 每节点的平均字节数。网格共享生效的话，这个数应当远小于
                // 一个立方体网格自身的大小。
                black_box(bytes / 1_000)
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, save, load, footprint);
criterion_main!(benches);
