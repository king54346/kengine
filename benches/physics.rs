//! 物理步进。
//!
//! 这是引擎里**单步开销最大**的一环：demo 里 19 个刚体就要 3.6~7.8 ms 一步，
//! 波动接近一倍。到底是场景变了还是求解器变慢了，光看 demo 日志分不出来。
//!
//! 三种场景分开量，因为它们压的是求解器的不同部分：
//!
//! - **自由落体**：几乎没有接触点，量的是积分与宽阶段的底噪；
//! - **堆叠**：接触点多且互相耦合，求解器迭代次数最吃紧的情况；
//! - **关节链**：约束求解，与接触是两套不同的代码路径。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kengine::prelude::*;
use std::hint::black_box;

/// 固定步长。物理必须定长步进，benchmark 里更是如此——
/// 步长一变，求解器的迭代次数和穿透深度全都跟着变，数字就没法比了。
const STEP: f32 = 1.0 / 60.0;

fn ground(world: &mut PhysicsWorld) {
    world.add_collider(&ColliderDesc::cuboid(Vec3::new(200.0, 0.5, 200.0)), None, 0);
}

/// 一堆互不接触的球，自由下落。
fn falling(count: usize) -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    ground(&mut world);

    let side = (count as f32).sqrt().ceil() as usize;
    for i in 0..count {
        let (x, z) = (i % side, i / side);
        let body = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(
                x as f32 * 4.0,
                50.0,
                z as f32 * 4.0,
            )),
            i as u128,
        );
        world.add_collider(&ColliderDesc::ball(0.5), Some(body), i as u128);
    }
    world
}

/// 几摞箱子，已经落稳。
///
/// 先跑 120 步让它安顿下来：正在下落的一摞和已经堆稳的一摞，
/// 接触点数量差一个量级，不预热就是在量「下落」而不是「堆叠」。
fn stacked(stacks: usize, height: usize) -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    ground(&mut world);

    let mut user_data = 1u128;
    for s in 0..stacks {
        for h in 0..height {
            let body = world.add_body(
                &RigidBodyDesc::dynamic().with_position(Vec3::new(
                    s as f32 * 3.0,
                    0.5 + h as f32 * 1.02,
                    0.0,
                )),
                user_data,
            );
            world.add_collider(
                &ColliderDesc::cuboid(Vec3::splat(0.5)),
                Some(body),
                user_data,
            );
            user_data += 1;
        }
    }

    for _ in 0..120 {
        world.step(STEP);
    }
    world
}

/// 一条用球关节连起来的链，顶端固定。
fn joint_chain(links: usize) -> PhysicsWorld {
    let mut world = PhysicsWorld::new();

    let mut previous = world.add_body(&RigidBodyDesc::fixed().with_position(Vec3::Y * 20.0), 0);
    world.add_collider(&ColliderDesc::ball(0.25), Some(previous), 0);

    for i in 1..=links {
        let body = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(0.0, 20.0 - i as f32, 0.0)),
            i as u128,
        );
        world.add_collider(&ColliderDesc::ball(0.25), Some(body), i as u128);
        world.add_joint(
            previous,
            body,
            &JointDesc::spherical(Vec3::NEG_Y * 0.5, Vec3::Y * 0.5, SphericalLimits::default()),
        );
        previous = body;
    }

    for _ in 0..60 {
        world.step(STEP);
    }
    world
}

fn physics_step(c: &mut Criterion) {
    let mut group = c.benchmark_group("physics_step");
    // 物理步进比场景操作慢两三个数量级，默认的采样数会让整轮跑很久。
    group.sample_size(30);

    for count in [64usize, 256, 1_024] {
        group.bench_with_input(BenchmarkId::new("falling", count), &count, |b, &count| {
            // 每次迭代都从同一个初态开始：让它一直落下去的话，
            // 后面的迭代会陆续撞到地面，量的东西中途就变了。
            b.iter_batched(
                || falling(count),
                |mut world| {
                    world.step(black_box(STEP));
                    black_box(world.stats().body_count)
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }

    for (stacks, height) in [(4usize, 5usize), (10, 10)] {
        let label = format!("{}x{}", stacks, height);
        group.bench_function(BenchmarkId::new("stacked", &label), |b| {
            b.iter_batched(
                || stacked(stacks, height),
                |mut world| {
                    world.step(black_box(STEP));
                    black_box(world.stats().body_count)
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }

    for links in [16usize, 64] {
        group.bench_with_input(BenchmarkId::new("joints", links), &links, |b, &links| {
            b.iter_batched(
                || joint_chain(links),
                |mut world| {
                    world.step(black_box(STEP));
                    black_box(world.stats().body_count)
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

fn queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("physics_query");

    // 查询在一个静止的世界里量：动着的世界每次迭代的 BVH 都不一样，
    // 量出来的方差会盖过要看的东西。
    let world = stacked(10, 10);

    group.bench_function("raycast/hit", |b| {
        let options = RayCastOptions::new(Vec3::new(0.0, 30.0, 0.0), Vec3::NEG_Y, 100.0);
        b.iter(|| black_box(world.cast_ray(black_box(&options))).is_some());
    });

    group.bench_function("raycast/miss", |b| {
        // 打空的情况必须单独量：它要走完整棵 BVH 才能确定「没打到」，
        // 通常比打中更慢，而打中的那条路径是提前退出的。
        let options = RayCastOptions::new(Vec3::new(500.0, 30.0, 0.0), Vec3::NEG_Y, 100.0);
        b.iter(|| black_box(world.cast_ray(black_box(&options))).is_some());
    });

    group.finish();
}

criterion_group!(benches, physics_step, queries);
criterion_main!(benches);
