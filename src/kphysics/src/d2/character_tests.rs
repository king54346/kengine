//! 2D 角色控制器的测试。
//!
//! 平台跳跃的手感全在这上面：上台阶、贴墙滑、斜坡站得住、跳跃由自己控制。

use super::*;
use crate::InteractionGroups;
use kmath::Vec2;

const HALF_HEIGHT: f32 = 0.5;
const RADIUS: f32 = 0.3;
/// 站在 y=0 的地面上时角色中心的高度。
const STAND_Y: f32 = HALF_HEIGHT + RADIUS;

fn world_with_ground() -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(
        &RigidBodyDesc::fixed().with_position(Vec2::new(0.0, -0.5)),
        0,
    );
    world.add_collider(&ColliderDesc::cuboid(Vec2::new(50.0, 0.5)), Some(ground), 0);
    world
}

fn add_character(world: &mut PhysicsWorld, position: Vec2) -> BodyHandle {
    let body = world.add_body(
        &RigidBodyDesc::kinematic_position().with_position(position),
        1,
    );
    world
        .add_collider(&ColliderDesc::capsule(HALF_HEIGHT, RADIUS), Some(body), 1)
        .expect("胶囊该建得出来");
    body
}

fn add_box(world: &mut PhysicsWorld, center: Vec2, half_extents: Vec2) {
    let body = world.add_body(&RigidBodyDesc::fixed().with_position(center), 2);
    world.add_collider(&ColliderDesc::cuboid(half_extents), Some(body), 2);
}

/// 跑若干帧：加重力、移动、落地清零。
fn walk(
    world: &mut PhysicsWorld,
    controller: &CharacterController,
    body: BodyHandle,
    horizontal: f32,
    frames: usize,
) -> CharacterMovement {
    let dt = 1.0 / 60.0;
    let mut vertical = 0.0_f32;
    let mut last = CharacterMovement {
        translation: Vec2::ZERO,
        grounded: false,
        sliding_down_slope: false,
    };

    for _ in 0..frames {
        vertical += -9.81 * dt;
        last = world.move_character(
            controller,
            body,
            Vec2::new(horizontal * dt, vertical * dt),
            dt,
        );
        if last.grounded {
            vertical = 0.0;
        }
        world.step(dt);
    }
    last
}

fn position(world: &PhysicsWorld, body: BodyHandle) -> Vec2 {
    world.body(body).expect("刚体还在").position()
}

#[test]
fn a_character_falls_and_lands() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, 5.0));

    let last = walk(&mut world, &CharacterController::default(), body, 0.0, 180);

    assert!(last.grounded, "该落地了");
    let y = position(&world, body).y;
    assert!((y - STAND_Y).abs() < 0.1, "该站在 {STAND_Y} 附近，实测 {y}");
}

#[test]
fn a_grounded_character_does_not_sink() {
    // 恰好贴地摆放——最自然也最坑的摆法。见 3D 那边同名测试的注释。
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    walk(&mut world, &CharacterController::default(), body, 0.0, 600);

    let y = position(&world, body).y;
    assert!(y > STAND_Y - 0.1, "陷下去了：{y}");
}

#[test]
fn a_character_walks_on_flat_ground() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    let controller = CharacterController::default();
    let dt = 1.0 / 60.0;
    let mut vertical = 0.0_f32;
    for frame in 0..60 {
        vertical += -9.81 * dt;
        let m = world.move_character(&controller, body, Vec2::new(2.0 * dt, vertical * dt), dt);
        if frame < 12 {
            let p = position(&world, body);
            println!(
                "PROBE 帧{frame} pos=({:.4},{:.4}) 位移=({:.5},{:.5}) 落地={} 睡着={}",
                p.x, p.y, m.translation.x, m.translation.y, m.grounded,
                world.body(body).unwrap().is_sleeping()
            );
        }
        if m.grounded { vertical = 0.0; }
        world.step(dt);
    }
    assert!(position(&world, body).x > 1.5, "走得太少");
}

#[test]
fn a_character_steps_over_a_low_step() {
    // 平台跳跃里最常见的地形：一级小台阶。上不去的话玩家得跳，
    // 那手感就完全不对了。
    let mut world = world_with_ground();
    add_box(&mut world, Vec2::new(2.0, 0.1), Vec2::new(1.0, 0.1));
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    walk(&mut world, &CharacterController::default(), body, 2.0, 90);

    let p = position(&world, body);
    assert!(p.x > 1.5, "卡在 x={}", p.x);
    assert!(p.y > STAND_Y + 0.1, "没抬起来，y={}", p.y);
}

#[test]
fn autostep_can_be_turned_off() {
    // 反证。
    let mut world = world_with_ground();
    add_box(&mut world, Vec2::new(2.0, 0.1), Vec2::new(1.0, 0.1));
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));
    let controller = CharacterController::default().without_autostep();

    walk(&mut world, &controller, body, 2.0, 90);

    assert!(
        position(&world, body).y < STAND_Y + 0.05,
        "关掉了却还是爬上去了"
    );
}

#[test]
fn a_character_is_stopped_by_a_wall() {
    let mut world = world_with_ground();
    add_box(&mut world, Vec2::new(2.0, 1.0), Vec2::new(0.2, 1.0));
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    walk(&mut world, &CharacterController::default(), body, 5.0, 300);

    assert!(position(&world, body).x < 2.0, "穿墙了");
}

#[test]
fn a_character_can_jump() {
    // 跳跃是「自己给一个向上的速度」，控制器不插手——这正是
    // 用运动学而不是动态刚体的理由。
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));
    let controller = CharacterController::default();
    let dt = 1.0 / 60.0;

    // 先站稳。
    walk(&mut world, &controller, body, 0.0, 30);
    let ground_y = position(&world, body).y;

    // 起跳。
    let mut vertical = 6.0_f32;
    let mut peak = ground_y;
    for _ in 0..120 {
        let movement = world.move_character(&controller, body, Vec2::new(0.0, vertical * dt), dt);
        world.step(dt);
        peak = peak.max(position(&world, body).y);
        vertical += -9.81 * dt;
        if movement.grounded && vertical < 0.0 {
            break;
        }
    }

    assert!(peak > ground_y + 1.0, "跳跃高度只有 {}", peak - ground_y);
    let landed = position(&world, body).y;
    assert!((landed - ground_y).abs() < 0.1, "没落回地面：{landed}");
}

#[test]
fn the_character_does_not_collide_with_itself() {
    let mut world = PhysicsWorld::new();
    let body = add_character(&mut world, Vec2::ZERO);

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec2::new(1.0, 0.0),
        1.0 / 60.0,
    );
    assert!(
        movement.translation.x > 0.5,
        "只走了 {}，多半撞上了自己",
        movement.translation.x
    );
}

#[test]
fn moving_works_before_the_first_step() {
    // 查询走的是广相 BVH，在 `step` 里维护。不自动刷新的话
    // 「加载完关卡立刻移动」会直接穿墙，而且不报错。
    let mut world = world_with_ground();
    add_box(&mut world, Vec2::new(1.0, 1.0), Vec2::new(0.2, 1.0));
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec2::new(5.0, 0.0),
        1.0 / 60.0,
    );
    assert!(movement.translation.x < 1.0, "还没步进过就穿墙了");
}

#[test]
fn computing_does_not_move_the_body() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));
    let before = position(&world, body);

    let movement = world.compute_character_movement(
        &CharacterController::default(),
        body,
        Vec2::new(1.0, 0.0),
        1.0 / 60.0,
        &mut |_| {},
    );
    world.step(1.0 / 60.0);

    assert!(movement.translation.x > 0.0);
    assert!((position(&world, body) - before).length() < 0.01, "只算不动，却挪了");
}

#[test]
fn collisions_are_reported() {
    let mut world = world_with_ground();
    add_box(&mut world, Vec2::new(1.0, 1.0), Vec2::new(0.2, 1.0));
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    let mut hits = Vec::new();
    world.compute_character_movement(
        &CharacterController::default(),
        body,
        Vec2::new(5.0, 0.0),
        1.0 / 60.0,
        &mut |c| hits.push(c),
    );

    assert!(!hits.is_empty(), "撞上了墙却没报告");
    for hit in &hits {
        assert!(hit.point.is_finite() && hit.normal.is_finite());
    }
}

#[test]
fn collision_groups_let_the_character_pass_through() {
    let mut world = world_with_ground();
    let wall = world.add_body(&RigidBodyDesc::fixed().with_position(Vec2::new(1.0, 1.0)), 2);
    world.add_collider(
        &ColliderDesc::cuboid(Vec2::new(0.2, 1.0))
            .with_collision_groups(InteractionGroups::new(0b01, 0b01)),
        Some(wall),
        2,
    );
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    let controller =
        CharacterController::default().with_groups(InteractionGroups::new(0b10, 0b10));
    let movement =
        world.move_character(&controller, body, Vec2::new(5.0, 0.0), 1.0 / 60.0);

    assert!(movement.translation.x > 4.0, "过滤组没生效");
}

#[test]
fn a_stale_handle_does_nothing() {
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, 2.0));
    world.remove_body(body);

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec2::new(1.0, 0.0),
        1.0 / 60.0,
    );
    assert_eq!(movement.translation, Vec2::ZERO);
}

#[test]
fn the_result_stays_finite_on_a_huge_step() {
    // NaN 位置会让整个物理世界失效，而且很难查到源头。
    let mut world = world_with_ground();
    let body = add_character(&mut world, Vec2::new(0.0, STAND_Y));

    let movement = world.move_character(
        &CharacterController::default(),
        body,
        Vec2::new(1000.0, 0.0),
        1.0 / 60.0,
    );
    assert!(movement.translation.is_finite());
    world.step(1.0 / 60.0);
    assert!(position(&world, body).is_finite());
}

#[test]
fn the_2d_and_3d_controllers_share_their_parameter_types() {
    // `Autostep` 和 `Length` 是共用的——同一个概念在两个维度里
    // 是同一件事，各定义一份只会让人以为它们不一样。
    let controller = CharacterController {
        autostep: Some(crate::Autostep {
            max_height: crate::Length::Absolute(0.4),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(controller.autostep.is_some());
}
