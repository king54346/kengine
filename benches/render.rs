//! 渲染器一帧的 CPU 时间：剔除、合批、上传、编码命令、提交。
//!
//! 用 [`Renderer::headless`] 起一个不开窗口的渲染器，走的是和屏幕帧一模一样的路径，
//! 只是最后不 `present`。每一轮只计 `render` 本身，等显卡做完的时间排除在外——
//! 这里量的是 CPU 侧，GPU 侧看剖析面板（F3）。
//!
//! 机器上没有可用的显卡适配器（CI 常见）时整组跳过，不报错。
//!
//! # 对照
//!
//! 按 `benches/README.md` 的规矩，同一轮里有一条完全不碰渲染器的对照
//! `render/control/sort`：它涨了多少，机器就慢了多少。下性能结论时报
//! 「物体档 / 对照」的比值，不报绝对数。
//!
//! `render/instanced/*` 是同一片方块的实例化版本（八个节点，方块全是实例），和 `render/plain/*` 对着看。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kengine::krender::Renderer;
use kengine::prelude::*;
use std::hint::black_box;
use std::time::{Duration, Instant};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// 一片方块，八种材质轮着用（合批要按材质分组，全一样的话量不到分组的开销）。
/// 相机斜着看向中心：一部分在视锥里、一部分在外，和真实场景一样两条路都走到。
fn scene(count: usize, shadows: bool) -> Scene {
    let mut scene = Scene::new();
    let mesh = Mesh::cube();
    let materials: Vec<Material> = (0..8)
        .map(|i| {
            let t = i as f32 / 7.0;
            Material::default().with_base_color(Vec4::new(0.2 + 0.8 * t, 0.5, 1.0 - 0.8 * t, 1.0))
        })
        .collect();
    let side = (count as f32).sqrt().ceil() as usize;
    for i in 0..count {
        let (x, z) = ((i % side) as f32, (i / side) as f32);
        scene.add_node(
            Node::new(format!("n{i}"))
                .with_mesh(mesh.clone())
                .with_material(materials[i % materials.len()].clone())
                .with_position(Vec3::new(x * 2.0, ((i * 7) % 5) as f32 * 0.3, z * 2.0)),
        );
    }
    let center = side as f32;
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default())
            .with_transform(Transform::looking_at(
                Vec3::new(center - 30.0, 25.0, center - 30.0),
                Vec3::new(center, 0.0, center),
                Vec3::Y,
            )),
    );
    let sun = if shadows {
        Light::directional().with_shadows()
    } else {
        Light::directional()
    };
    scene.add_node(
        Node::new("Sun")
            .with_light(sun)
            .with_transform(Transform::looking_at(
                Vec3::new(4.0, 10.0, 2.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    scene.update();
    scene
}

/// 同一片方块，改成实例化：八种材质各一个节点，方块全是实例。和 [`scene`] 逐个对得上（位置、材质）。
fn instanced_scene(count: usize) -> Scene {
    let mut scene = scene(0, false);
    let materials: Vec<Material> = (0..8)
        .map(|i| {
            let t = i as f32 / 7.0;
            Material::default().with_base_color(Vec4::new(0.2 + 0.8 * t, 0.5, 1.0 - 0.8 * t, 1.0))
        })
        .collect();
    let side = (count as f32).sqrt().ceil() as usize;
    let mut groups: Vec<Vec<Instance>> = vec![Vec::new(); materials.len()];
    for i in 0..count {
        let (x, z) = ((i % side) as f32, (i / side) as f32);
        groups[i % materials.len()].push(Instance::at(Vec3::new(
            x * 2.0,
            ((i * 7) % 5) as f32 * 0.3,
            z * 2.0,
        )));
    }
    for (material, instances) in materials.into_iter().zip(groups) {
        scene.add_node(
            Node::new("Group")
                .with_mesh(Mesh::cube())
                .with_material(material)
                .with_instances(instances),
        );
    }
    // 相机挪到和 `scene(count)` 一样的位置。
    let center = side as f32;
    if let Some(camera) = scene.find_by_name("Camera") {
        scene[camera].transform = Transform::looking_at(
            Vec3::new(center - 30.0, 25.0, center - 30.0),
            Vec3::new(center, 0.0, center),
            Vec3::Y,
        );
    }
    scene.update();
    scene
}

fn render(c: &mut Criterion) {
    let Some(mut renderer) = pollster::block_on(Renderer::headless(WIDTH, HEIGHT)) else {
        eprintln!("没有可用的显卡适配器，跳过 render 基准");
        return;
    };
    let mut ui = Ui::new();
    ui.begin_frame(Vec2::new(WIDTH as f32, HEIGHT as f32), 1.0);
    ui.end_frame();

    let mut group = c.benchmark_group("render");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(4));

    // 对照：固定种子的一次排序，跟渲染器无关。
    group.bench_function("control/sort", |b| {
        let mut rng = kengine::kmath::Rng::new(7);
        let data: Vec<u32> = (0..200_000).map(|_| rng.next_u32()).collect();
        b.iter(|| {
            let mut v = data.clone();
            v.sort_unstable();
            black_box(v[v.len() / 2])
        });
    });

    for (label, shadows) in [("plain", false), ("shadow", true), ("instanced", false)] {
        for count in [1_000usize, 10_000, 50_000] {
            let scene = if label == "instanced" {
                instanced_scene(count)
            } else {
                scene(count, shadows)
            };
            // 预热：第一帧要建管线、上传网格，不算。
            for _ in 0..3 {
                renderer.render(&scene, &ui, &[]);
            }
            renderer.wait_idle();
            group.bench_with_input(BenchmarkId::new(label, count), &count, |b, _| {
                b.iter_custom(|iterations| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iterations {
                        let start = Instant::now();
                        black_box(renderer.render(&scene, &ui, &[]));
                        total += start.elapsed();
                        // 不等的话命令越积越多，后面的 `render` 会被队列反压拖慢。
                        renderer.wait_idle();
                    }
                    total
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, render);
criterion_main!(benches);
