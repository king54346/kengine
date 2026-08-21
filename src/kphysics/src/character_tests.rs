//! 角色控制器的测试。
//!
//! 测的是模块文档里列的那四件「刚体硬凑做不到」的事：爬台阶、站住斜坡、
//! 贴墙滑、以及位移完全由调用方控制。

use crate::*;
use kmath::Vec3;

/// 角色胶囊：半高 0.5、半径 0.3，所以总高 1.6、脚底离中心 0.8。
const HALF_HEIGHT: f32 = 0.5;
const RADIUS: f32 = 0.3;
/// 站在 y=0 的地面上时，角色中心应当在的高度。
const STAND_Y: f32 = HALF_HEIGHT + RADIUS;

/// 一个带地面的世界。地面上表面在 y=0。
fn world_with_ground() -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(
        &RigidBodyDesc::fixed().with_position(Vec3::new(0.0, -0.5, 0.0)),
        0,
    );
    world.add_collider(
        &ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)),
        Some(ground),
        0,
    );
    world
}

/// 加一个角色，返回它的刚体句柄。
fn add_character(world: &mut PhysicsWorld, position: Vec3) -> BodyHandle {
    let body = world.add_body(
        &RigidBodyDesc::kinematic_position_based().with_position(position),
        1,
    );
    world
        .add_collider(
            &ColliderDesc::capsule_y(HALF_HEIGHT, RADIUS),
            Some(body),
            1,
        )
        .expect("胶囊该建得出来");
    body
}

/// 加一个固定的盒子。
fn add_box(world: &mut PhysicsWorld, center: Vec3, half_extents: Vec3) {
    let body = world.add_body(&RigidBodyDesc::fixed().with_position(center), 2);
    world.add_collider(&ColliderDesc::cuboid(half_extents), Some(body), 2);
}

/// 跑若干帧：每帧加重力、移动、落地清零。返回最后一次的结果。
fn walk(
    world: &mut PhysicsWorld,
    controller: &CharacterController,
    body: BodyHandle,
    horizontal: Vec3,
    frames: usize,
) -> CharacterMovement {
    let dt = 1.0 / 60.0;
    let mut vertical = 0.0_f32;
    let mut last = CharacterMovement {
        translation: Vec3::ZERO,
        grounded: false,
        sliding_down_slope: false,
    };

    for _ in 0..frames {
        vertical += -9.81 * dt;
        let desired = horizontal * dt + Vec3::new(0.0, vertical * dt, 0.0);
        last = world.move_character(controller, body, desired, dt);
        if last.grounded {
            // 不清零的话下坠速度越积越大，离开地面的瞬间角色会像被弹弓射出去。
            vertical = 0.0;
        }
        world.step(dt);
    }
    last
}

fn position(world: &PhysicsWorld, body: BodyHandle) -> Vec3 {
    world.body(body).expect("刚体还在").position()
}

// ── 基本：落地与站住 ──

#[test]
fn a_character_falls_and_lands() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, 5.0, 0.0));
    let controller = CharacterController::default();

    let last = walk(&mut world, &controller, body, Vec3::ZERO, 180);

    assert!(last.grounded, "该落地了");
    let y = position(&world, body).y;
    assert!((y - STAND_Y).abs() < 0.1, "该站在 {STAND_Y} 附近，实测 {y}");
}

#[test]
fn a_grounded_character_does_not_sink() {
    // 站着不动时不该慢慢往下陷。每帧的重力会被地面吃掉，
    // 但吃不干净的话几百帧下来就沉进地里了。
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y + 0.05, 0.0));
    let controller = CharacterController::default();

    let dt = 1.0 / 60.0;
    let mut vertical = 0.0_f32;
    for frame in 0..600 {
        vertical += -9.81 * dt;
        let m = world.move_character(&controller, body, Vec3::new(0.0, vertical * dt, 0.0), dt);
        if frame < 5 || frame % 150 == 0 {
            println!(
                "PROBE 帧{frame} y={:.5} 位移y={:.6} 落地={} 想走y={:.6}",
                position(&world, body).y, m.translation.y, m.grounded, vertical * dt
            );
        }
        if m.grounded { vertical = 0.0; }
        world.step(dt);
    }

    let y = position(&world, body).y;
    assert!(y > STAND_Y - 0.1, "陷下去了：{y}");
}

#[test]
fn a_character_walks_on_flat_ground() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default();

    walk(&mut world, &controller, body, Vec3::new(2.0, 0.0, 0.0), 60);

    let x = position(&world, body).x;
    // 一秒 2 米/秒，该走出约 2 米。
    assert!(x > 1.5, "只走了 {x} 米");
}

// ── 台阶：刚体硬凑做不到的第一件事 ──

#[test]
fn a_character_steps_over_a_low_step() {
    // 一级 20 厘米的台阶。用动态刚体的话胶囊会被卡住，
    // 因为求解器看到的是法线朝侧面的接触，只会把角色往外推。
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(2.0, 0.1, 0.0),
        Vec3::new(1.0, 0.1, 2.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default();

    walk(&mut world, &controller, body, Vec3::new(2.0, 0.0, 0.0), 90);

    let p = position(&world, body);
    assert!(p.x > 1.5, "没走到台阶上，卡在 x={}", p.x);
    assert!(p.y > STAND_Y + 0.1, "没抬起来，y={}（台阶高 0.2）", p.y);
}

#[test]
fn autostep_can_be_turned_off() {
    // 反证：关掉之后同一级台阶就该拦住角色。没有这一条的话，
    // 上一条在「台阶根本没生效」的情况下也会通过。
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(2.0, 0.1, 0.0),
        Vec3::new(1.0, 0.1, 2.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default().without_autostep();

    walk(&mut world, &controller, body, Vec3::new(2.0, 0.0, 0.0), 90);

    let p = position(&world, body);
    assert!(p.y < STAND_Y + 0.05, "关掉了却还是爬上去了，y={}", p.y);
}

#[test]
fn a_character_is_stopped_by_a_tall_step() {
    // 台阶高过 max_height 就该拦住。默认是相对高度 0.25，
    // 角色总高 1.6，所以约 0.4 米。这里给一个 1 米的。
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(2.0, 0.5, 0.0),
        Vec3::new(1.0, 0.5, 2.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default();

    walk(&mut world, &controller, body, Vec3::new(2.0, 0.0, 0.0), 90);

    let p = position(&world, body);
    assert!(p.y < STAND_Y + 0.2, "爬上了一米高的台阶，y={}", p.y);
    // 会被挡在盒子前面（盒子左边缘在 x=1）。
    assert!(p.x < 1.0, "穿过去了，x={}", p.x);
}

// ── 墙：贴墙滑 ──

#[test]
fn a_character_slides_along_a_wall() {
    // 斜着撞墙时该贴着墙滑过去，而不是硬停或者弹开。
    let mut world = world_with_ground();
    // 一堵沿 Z 轴的墙，在 x=2。
    add_box(
        &mut world,
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(0.2, 1.0, 10.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default();

    // 斜着往墙上走：x 和 z 各一半。
    walk(
        &mut world,
        &controller,
        body,
        Vec3::new(2.0, 0.0, 2.0),
        120,
    );

    let p = position(&world, body);
    assert!(p.x < 2.0, "穿墙了，x={}", p.x);
    assert!(p.z > 1.5, "撞墙之后没沿墙滑，z={}", p.z);
}

#[test]
fn sliding_can_be_turned_off() {
    // 反证。关掉之后撞墙就硬停，z 方向也走不动多少。
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(0.2, 1.0, 10.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default().without_sliding();

    let with_slide = {
        let mut w = world_with_ground();
        add_box(&mut w, Vec3::new(2.0, 1.0, 0.0), Vec3::new(0.2, 1.0, 10.0));
        let b = add_character(&mut w, Vec3::new(0.0, STAND_Y, 0.0));
        walk(
            &mut w,
            &CharacterController::default(),
            b,
            Vec3::new(2.0, 0.0, 2.0),
            120,
        );
        position(&w, b).z
    };

    walk(
        &mut world,
        &controller,
        body,
        Vec3::new(2.0, 0.0, 2.0),
        120,
    );
    let without_slide = position(&world, body).z;

    assert!(
        without_slide < with_slide,
        "关掉滑动之后反而走得更远：{without_slide} vs {with_slide}"
    );
}

#[test]
fn a_character_does_not_pass_through_a_wall() {
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(0.2, 1.0, 10.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let controller = CharacterController::default();

    // 一路顶着墙走很久。
    walk(
        &mut world,
        &controller,
        body,
        Vec3::new(5.0, 0.0, 0.0),
        300,
    );

    assert!(position(&world, body).x < 2.0, "穿墙了");
}

// ── 坡度 ──

#[test]
fn a_character_climbs_a_gentle_slope() {
    // 20° 的坡，默认能爬 45°。
    let mut world = world_with_ground();
    let angle = 20.0_f32.to_radians();
    let slope = world.add_body(
        &RigidBodyDesc::fixed()
            .with_position(Vec3::new(5.0, 0.0, 0.0))
            .with_rotation(kmath::Quat::from_rotation_z(angle)),
        3,
    );
    world.add_collider(
        &ColliderDesc::cuboid(Vec3::new(5.0, 0.2, 5.0)),
        Some(slope),
        3,
    );

    let body = add_character(&mut world, Vec3::new(1.0, STAND_Y + 0.5, 0.0));
    let controller = CharacterController::default();

    let start = position(&world, body);
    walk(&mut world, &controller, body, Vec3::new(2.0, 0.0, 0.0), 120);
    let end = position(&world, body);

    assert!(end.x > start.x + 1.0, "没往前走，x {} → {}", start.x, end.x);
    assert!(end.y > start.y, "爬坡时没升高，y {} → {}", start.y, end.y);
}

#[test]
fn the_max_slope_can_be_lowered() {
    let controller = CharacterController::default().with_max_slope(10.0_f32.to_radians());
    assert!((controller.max_slope_climb_angle - 10.0_f32.to_radians()).abs() < 1e-6);
    // 下滑角度会被一起夹住——两者反过来的话中间那段角色会卡住不动。
    assert!(controller.min_slope_slide_angle <= controller.max_slope_climb_angle);
}

// ── 退化输入 ──

#[test]
fn moving_a_body_without_a_collider_does_nothing() {
    // 没有碰撞体就没有形状，无从扫掠。返回零位移而不是让它自由穿墙。
    let mut world = world_with_ground();
    let body = world.add_body(&RigidBodyDesc::kinematic_position_based(), 9);
    let controller = CharacterController::default();

    // debug 构建下这里会 debug_assert，所以只在 release 里跑这一段。
    if cfg!(debug_assertions) {
        return;
    }
    let movement = world.move_character(&controller, body, Vec3::new(1.0, 0.0, 0.0), 1.0 / 60.0);
    assert_eq!(movement.translation, Vec3::ZERO);
    assert!(!movement.grounded);
}

#[test]
fn moving_a_stale_handle_does_nothing() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, 2.0, 0.0));
    world.remove_body(body);

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec3::new(1.0, 0.0, 0.0),
        1.0 / 60.0,
    );
    assert_eq!(movement.translation, Vec3::ZERO);
}

#[test]
fn a_zero_desired_translation_keeps_the_character_put() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let before = position(&world, body);

    world.move_character(
        &CharacterController::default(),
        body,
        Vec3::ZERO,
        1.0 / 60.0,
    );
    world.step(1.0 / 60.0);

    assert!((position(&world, body) - before).length() < 0.01);
}

#[test]
fn the_result_stays_finite_on_a_huge_step() {
    // 一帧走一千米。不该产生 NaN——NaN 位置会让整个物理世界失效，
    // 而且很难查到源头。
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec3::new(1000.0, 0.0, 0.0),
        1.0 / 60.0,
    );
    assert!(movement.translation.is_finite(), "{:?}", movement.translation);
    world.step(1.0 / 60.0);
    assert!(position(&world, body).is_finite());
}

// ── 移动前先刷新查询结构 ──

#[test]
fn moving_works_before_the_first_step() {
    // 查询走的是广相的 BVH，而 BVH 在 `step` 里维护。不自动刷新的话，
    // 「加载完关卡立刻移动角色」会直接穿过所有墙——而且不报任何错。
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.2, 1.0, 5.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));

    // 一次 step 都没跑过就移动。
    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec3::new(5.0, 0.0, 0.0),
        1.0 / 60.0,
    );

    assert!(
        movement.translation.x < 1.0,
        "还没步进过就移动，直接穿墙了：{}",
        movement.translation.x
    );
}

// ── 只算不动 ──

#[test]
fn computing_does_not_move_the_body() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));
    let before = position(&world, body);

    let movement = world.compute_character_movement(
        &CharacterController::default(),
        body,
        Vec3::new(1.0, 0.0, 0.0),
        1.0 / 60.0,
        &mut |_| {},
    );
    world.step(1.0 / 60.0);

    assert!(movement.translation.x > 0.0, "算出来的位移是零");
    assert!(
        (position(&world, body) - before).length() < 0.01,
        "只算不动，却把刚体挪了"
    );
}

#[test]
fn collisions_are_reported() {
    // 撞到什么就播什么音效、推箱子，都靠这个回调。
    let mut world = world_with_ground();
    add_box(
        &mut world,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.2, 1.0, 5.0),
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));

    let mut hits = Vec::new();
    world.compute_character_movement(
        &CharacterController::default(),
        body,
        Vec3::new(5.0, 0.0, 0.0),
        1.0 / 60.0,
        &mut |collision| hits.push(collision),
    );

    assert!(!hits.is_empty(), "撞上了墙却没报告");
    for hit in &hits {
        assert!(hit.point.is_finite(), "接触点是 {:?}", hit.point);
        assert!(hit.normal.is_finite(), "法线是 {:?}", hit.normal);
        assert!(hit.distance >= 0.0);
    }
}

#[test]
fn the_character_does_not_collide_with_itself() {
    // 不排除自己的碰撞体的话，第一次扫掠就撞上自己，角色一步也走不动。
    let mut world = PhysicsWorld::new();
    let body = add_character(&mut world, Vec3::new(0.0, 0.0, 0.0));

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec3::new(1.0, 0.0, 0.0),
        1.0 / 60.0,
    );
    assert!(
        movement.translation.x > 0.5,
        "只走了 {}，多半是撞上了自己",
        movement.translation.x
    );
}

// ── 过滤组 ──

#[test]
fn collision_groups_let_the_character_pass_through() {
    let mut world = world_with_ground();
    // 一堵只属于组 0b01 的墙。
    let wall = world.add_body(&RigidBodyDesc::fixed().with_position(Vec3::new(1.0, 1.0, 0.0)), 2);
    world.add_collider(
        &ColliderDesc::cuboid(Vec3::new(0.2, 1.0, 5.0))
            .with_groups(InteractionGroups::new(0b01, 0b01)),
        Some(wall),
        2,
    );
    let body = add_character(&mut world, Vec3::new(0.0, STAND_Y, 0.0));

    // 角色只和 0b10 交互，和那堵墙互不相干。
    let controller =
        CharacterController::default().with_groups(InteractionGroups::new(0b10, 0b10));
    let movement =
        world.move_character(&controller, body, Vec3::new(5.0, 0.0, 0.0), 1.0 / 60.0);

    assert!(
        movement.translation.x > 4.0,
        "过滤组没生效，被墙挡住了：{}",
        movement.translation.x
    );
}
