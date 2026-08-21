//! 2D 物理的测试。

use super::*;
use crate::{InteractionGroups, RigidBodyType};
use kmath::Vec2;

/// 一个带地面的世界，地面中心在 y=0、半高 0.5，所以上表面在 y=0.5。
fn world_with_ground() -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(&ColliderDesc::cuboid(Vec2::new(50.0, 0.5)), Some(ground), 0)
        .expect("地面该建得出来");
    world
}

fn drop_ball(world: &mut PhysicsWorld, from: Vec2, radius: f32) -> BodyHandle {
    let body = world.add_body(&RigidBodyDesc::dynamic().with_position(from), 1);
    world
        .add_collider(&ColliderDesc::ball(radius), Some(body), 1)
        .expect("球该建得出来");
    body
}

fn simulate(world: &mut PhysicsWorld, steps: usize) {
    for _ in 0..steps {
        world.step(1.0 / 60.0);
    }
}

#[test]
fn gravity_pulls_along_negative_y() {
    // 2D 里 Y 轴朝上是数学惯例，也和引擎的精灵坐标一致。
    // 弄反的话所有东西会往天上掉。
    let world = PhysicsWorld::new();
    assert!(world.gravity().y < 0.0);
    assert_eq!(world.gravity().x, 0.0);
}

#[test]
fn a_ball_falls_and_lands_on_the_ground() {
    let mut world = world_with_ground();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 10.0), 0.5);
    simulate(&mut world, 180);

    let y = world.body(ball).unwrap().position().y;
    // 地面上表面在 0.5，球心该停在 1.0 附近。
    assert!((y - 1.0).abs() < 0.15, "球停在 y={y}，该在 1.0 附近");
}

#[test]
fn a_fixed_body_does_not_move() {
    let mut world = world_with_ground();
    let wall = world.add_body(
        &RigidBodyDesc::fixed().with_position(Vec2::new(5.0, 5.0)),
        2,
    );
    world.add_collider(&ColliderDesc::cuboid(Vec2::splat(1.0)), Some(wall), 2);
    simulate(&mut world, 60);

    assert_eq!(world.body(wall).unwrap().position(), Vec2::new(5.0, 5.0));
}

#[test]
fn locked_rotation_keeps_a_box_upright() {
    // 平台跳跃的主角基本都要这个，否则一撞墙就开始打转。
    let mut world = world_with_ground();
    let body = world.add_body(
        &RigidBodyDesc::dynamic()
            .with_position(Vec2::new(0.0, 5.0))
            .with_locked_rotation(),
        3,
    );
    world.add_collider(&ColliderDesc::cuboid(Vec2::splat(0.5)), Some(body), 3);
    world
        .body_mut(body)
        .unwrap()
        .apply_torque_impulse(50.0, true);
    simulate(&mut world, 60);

    let rotation = world.body(body).unwrap().rotation();
    assert!(rotation.abs() < 1e-3, "旋转被锁住了却转了 {rotation} 弧度");
}

#[test]
fn rotation_is_free_when_not_locked() {
    // 反证：不锁的时候同样的扭矩该让它转起来。没有这一条的话，
    // 上一条在「扭矩根本没生效」的情况下也会通过。
    let mut world = world_with_ground();
    let body = world.add_body(
        &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 5.0)),
        3,
    );
    world.add_collider(&ColliderDesc::cuboid(Vec2::splat(0.5)), Some(body), 3);
    world
        .body_mut(body)
        .unwrap()
        .apply_torque_impulse(50.0, true);
    simulate(&mut world, 10);

    assert!(world.body(body).unwrap().angvel().abs() > 0.1, "扭矩没生效");
}

#[test]
fn an_impulse_changes_velocity_immediately() {
    // 力要乘以时间步才变成速度，冲量直接就是动量。
    let mut world = PhysicsWorld::new();
    let body = world.add_body(&RigidBodyDesc::dynamic(), 0);
    world.add_collider(&ColliderDesc::ball(0.5), Some(body), 0);

    let mass = world.body(body).unwrap().mass();
    world
        .body_mut(body)
        .unwrap()
        .apply_impulse(Vec2::new(mass * 3.0, 0.0), true);
    world.step(1.0 / 60.0);

    let vx = world.body(body).unwrap().linvel().x;
    assert!((vx - 3.0).abs() < 0.2, "冲量该给出 3 m/s，实测 {vx}");
}

#[test]
fn gravity_scale_zero_makes_a_body_float() {
    let mut world = PhysicsWorld::new();
    let body = world.add_body(
        &RigidBodyDesc::dynamic()
            .with_position(Vec2::new(0.0, 10.0))
            .with_gravity_scale(0.0),
        0,
    );
    world.add_collider(&ColliderDesc::ball(0.5), Some(body), 0);
    simulate(&mut world, 120);

    assert!((world.body(body).unwrap().position().y - 10.0).abs() < 1e-3);
}

#[test]
fn a_sensor_reports_overlap_without_pushing() {
    // 传感器只报告重叠，不产生碰撞响应——触发区用它。
    let mut world = PhysicsWorld::new();
    let trigger = world.add_body(&RigidBodyDesc::fixed(), 100);
    world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(5.0, 0.5)).as_sensor(),
            Some(trigger),
            100,
        )
        .unwrap();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 5.0), 0.5);

    let mut saw_event = false;
    for _ in 0..300 {
        world.step(1.0 / 60.0);
        if world
            .collision_events()
            .iter()
            .any(|e| e.sensor && e.started)
        {
            saw_event = true;
        }
    }

    assert!(saw_event, "传感器没报告重叠");
    assert!(
        world.body(ball).unwrap().position().y < -5.0,
        "传感器把球挡住了"
    );
}

#[test]
fn collision_groups_can_make_things_pass_through() {
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(50.0, 0.5))
                .with_collision_groups(InteractionGroups::new(0b01, 0b01)),
            Some(ground),
            0,
        )
        .unwrap();

    // 成员位 0b10、只和 0b10 交互——和地面互不相干。
    let ball = world.add_body(
        &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 5.0)),
        1,
    );
    world
        .add_collider(
            &ColliderDesc::ball(0.5).with_collision_groups(InteractionGroups::new(0b10, 0b10)),
            Some(ball),
            1,
        )
        .unwrap();
    simulate(&mut world, 120);

    assert!(
        world.body(ball).unwrap().position().y < -1.0,
        "过滤组没生效，球被地面挡住了"
    );
}

#[test]
fn a_raycast_hits_the_ground() {
    let mut world = world_with_ground();
    let hit = world
        .cast_ray(&RayCastOptions {
            origin: Vec2::new(0.0, 10.0),
            direction: Vec2::new(0.0, -1.0),
            ..Default::default()
        })
        .expect("该打到地面");

    // 地面上表面在 y=0.5，从 y=10 往下走 9.5。
    assert!(
        (hit.time_of_impact - 9.5).abs() < 0.05,
        "距离 {}",
        hit.time_of_impact
    );
    assert!(hit.normal.y > 0.9, "地面法线该朝上，实测 {:?}", hit.normal);
}

#[test]
fn a_raycast_works_before_the_first_step() {
    // 查询走的是广相的 BVH，而 BVH 在 step 里维护。不自动刷新的话
    // 「加载关卡后立刻检测地面」会静默返回 None——既不报错也不 panic，
    // 是最难查的那种问题。
    let mut world = world_with_ground();
    assert!(
        world
            .cast_ray(&RayCastOptions {
                origin: Vec2::new(0.0, 10.0),
                direction: Vec2::new(0.0, -1.0),
                ..Default::default()
            })
            .is_some(),
        "还没步进过就查询，静默返回了空"
    );
}

#[test]
fn a_raycast_misses_when_pointing_away() {
    let mut world = world_with_ground();
    assert!(
        world
            .cast_ray(&RayCastOptions {
                origin: Vec2::new(0.0, 10.0),
                direction: Vec2::new(0.0, 1.0),
                ..Default::default()
            })
            .is_none()
    );
}

#[test]
fn a_raycast_respects_max_distance() {
    let mut world = world_with_ground();
    assert!(
        world
            .cast_ray(&RayCastOptions {
                origin: Vec2::new(0.0, 10.0),
                direction: Vec2::new(0.0, -1.0),
                // 地面在 9.5 处，只走 5 该打不到。
                max_distance: 5.0,
                ..Default::default()
            })
            .is_none()
    );
}

#[test]
fn point_queries_find_the_collider_under_a_point() {
    let mut world = world_with_ground();
    assert_eq!(world.colliders_at_point(Vec2::ZERO).len(), 1);
    assert!(world.colliders_at_point(Vec2::new(0.0, 50.0)).is_empty());
}

#[test]
fn removing_a_body_removes_its_colliders() {
    // 不一起删的话它们会变成没有归属的幽灵碰撞体，仍然参与检测。
    let mut world = world_with_ground();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 10.0), 0.5);
    assert_eq!(world.collider_count(), 2);

    world.remove_body(ball);
    assert_eq!(world.body_count(), 1);
    assert_eq!(world.collider_count(), 1);
    assert!(world.body(ball).is_none());
}

#[test]
fn a_stale_handle_returns_none_instead_of_panicking() {
    let mut world = world_with_ground();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 10.0), 0.5);
    world.remove_body(ball);

    assert!(world.body(ball).is_none());
    assert!(world.body_mut(ball).is_none());
}

#[test]
fn a_degenerate_shape_is_rejected() {
    // 退化形状塞给求解器会算出 NaN，然后整个世界飞出去。
    let mut world = PhysicsWorld::new();
    let body = world.add_body(&RigidBodyDesc::dynamic(), 0);

    // 共线的点构不成凸多边形。
    let collinear = ColliderDesc::convex_polygon(vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(2.0, 0.0),
    ]);
    assert!(!collinear.is_valid());
    assert!(world.add_collider(&collinear, Some(body), 0).is_none());

    // 一个点连不成折线。
    let single = ColliderDesc::polyline(vec![Vec2::ZERO]);
    assert!(!single.is_valid());
    assert!(world.add_collider(&single, Some(body), 0).is_none());

    // 零法线的半平面。
    let zero_normal = ColliderDesc::half_space(Vec2::ZERO);
    assert!(!zero_normal.is_valid());
    assert!(world.add_collider(&zero_normal, Some(body), 0).is_none());

    assert_eq!(world.collider_count(), 0);
}

#[test]
fn a_valid_convex_polygon_is_accepted() {
    // 反证：不共线的点该建得出来。
    let square = ColliderDesc::convex_polygon(vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(0.0, 1.0),
    ]);
    assert!(square.is_valid());
}

#[test]
fn a_polyline_can_serve_as_terrain() {
    // 2D 关卡的地形轮廓。
    let mut world = PhysicsWorld::new();
    let terrain = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(
            &ColliderDesc::polyline(vec![
                Vec2::new(-10.0, 0.0),
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 3.0),
            ]),
            Some(terrain),
            0,
        )
        .expect("折线该建得出来");

    let ball = drop_ball(&mut world, Vec2::new(-5.0, 5.0), 0.5);
    simulate(&mut world, 240);

    let y = world.body(ball).unwrap().position().y;
    assert!((y - 0.5).abs() < 0.2, "球该停在折线上，实测 y={y}");
}

#[test]
fn restitution_makes_a_ball_bounce() {
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(50.0, 0.5)).with_restitution(0.9),
            Some(ground),
            0,
        )
        .unwrap();

    let body = world.add_body(
        &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 5.0)),
        1,
    );
    world
        .add_collider(
            &ColliderDesc::ball(0.5).with_restitution(0.9),
            Some(body),
            1,
        )
        .unwrap();

    // 落地之后应该反弹回来，中途出现过正的向上速度。
    let mut bounced = false;
    for _ in 0..300 {
        world.step(1.0 / 60.0);
        if world.body(body).unwrap().linvel().y > 1.0 {
            bounced = true;
            break;
        }
    }
    assert!(bounced, "弹性 0.9 却没弹起来");
}

#[test]
fn a_kinematic_body_pushes_without_being_pushed() {
    let mut world = PhysicsWorld::new();
    let platform = world.add_body(&RigidBodyDesc::kinematic_position(), 0);
    world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(5.0, 0.5)),
            Some(platform),
            0,
        )
        .unwrap();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 3.0), 0.5);

    // 先让球落到平台上。
    simulate(&mut world, 120);
    let resting = world.body(ball).unwrap().position();

    // 平台往上抬，球该被顶上去，平台自己不受影响。
    for i in 1..=60 {
        world
            .body_mut(platform)
            .unwrap()
            .set_next_kinematic_position(Vec2::new(0.0, i as f32 * 0.02), 0.0);
        world.step(1.0 / 60.0);
    }

    assert!(
        (world.body(platform).unwrap().position().y - 1.2).abs() < 0.05,
        "运动学刚体没走到目标位置"
    );
    assert!(
        world.body(ball).unwrap().position().y > resting.y + 0.5,
        "球没被平台顶上去"
    );
}

#[test]
fn disabling_the_world_freezes_everything() {
    let mut world = world_with_ground();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 10.0), 0.5);
    world.set_enabled(false);
    simulate(&mut world, 120);

    assert_eq!(world.body(ball).unwrap().position(), Vec2::new(0.0, 10.0));
}

#[test]
fn a_non_positive_time_step_is_ignored() {
    // 负的时间步会让积分器往回走，0 会让一堆除法变成除以零。
    let mut world = world_with_ground();
    let ball = drop_ball(&mut world, Vec2::new(0.0, 10.0), 0.5);

    world.step(0.0);
    world.step(-1.0);

    let position = world.body(ball).unwrap().position();
    assert_eq!(position, Vec2::new(0.0, 10.0));
    assert!(position.is_finite());
}

#[test]
fn user_data_survives_the_round_trip() {
    // kscene 往里塞节点句柄，靠它把模拟结果同步回场景图。
    let mut world = PhysicsWorld::new();
    let body = world.add_body(&RigidBodyDesc::dynamic(), 0xdead_beef);
    let collider = world
        .add_collider(&ColliderDesc::ball(0.5), Some(body), 0xcafe)
        .unwrap();

    assert_eq!(world.body(body).unwrap().user_data(), 0xdead_beef);
    assert_eq!(world.collider(collider).unwrap().user_data(), 0xcafe);
}

#[test]
fn body_type_round_trips() {
    let mut world = PhysicsWorld::new();
    for expected in [
        RigidBodyType::Dynamic,
        RigidBodyType::Fixed,
        RigidBodyType::KinematicPositionBased,
        RigidBodyType::KinematicVelocityBased,
    ] {
        let body = world.add_body(
            &RigidBodyDesc {
                body_type: expected,
                ..Default::default()
            },
            0,
        );
        assert_eq!(world.body(body).unwrap().body_type(), expected);
    }
}

#[test]
fn the_2d_world_is_independent_of_the_3d_one() {
    // 两个世界互不感知。一个 2D 刚体永远不会撞到一个 3D 刚体。
    let mut world2d = PhysicsWorld::new();
    let mut world3d = crate::PhysicsWorld::new();

    world2d.add_body(&RigidBodyDesc::dynamic(), 0);
    assert_eq!(world2d.body_count(), 1);
    assert_eq!(world3d.body_count(), 0);

    world3d.add_body(&crate::RigidBodyDesc::dynamic(), 0);
    assert_eq!(world2d.body_count(), 1);
    assert_eq!(world3d.body_count(), 1);
}

#[test]
fn a_stack_of_boxes_stays_stacked() {
    // 求解器稳定性的粗略检验：塌了的话说明接触约束没收敛。
    let mut world = world_with_ground();
    let mut boxes = Vec::new();
    for i in 0..5 {
        let body = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 1.0 + i as f32 * 1.01)),
            i as u128,
        );
        world
            .add_collider(&ColliderDesc::cuboid(Vec2::splat(0.5)), Some(body), 0)
            .unwrap();
        boxes.push(body);
    }
    simulate(&mut world, 300);

    for (i, body) in boxes.iter().enumerate() {
        let position = world.body(*body).unwrap().position();
        assert!(position.is_finite(), "第 {i} 个盒子的位置是 {position:?}");
        assert!(
            position.x.abs() < 1.0,
            "第 {i} 个盒子横向滑出了 {}",
            position.x
        );
    }
}
