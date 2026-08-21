//! 碰撞事件的测试。单独一个文件是因为它们都要围绕「事件默认关着」
//! 这一条设计做正反两面的验证。

use super::*;
use kmath::Vec2;

fn stack(emit: bool, sensor: bool) -> Vec<CollisionEvent2d> {
    let mut world = PhysicsWorld::new();

    let mut ground_desc = ColliderDesc::cuboid(Vec2::new(50.0, 0.5));
    ground_desc.emit_collision_events = emit;
    ground_desc.sensor = sensor;
    let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
    world.add_collider(&ground_desc, Some(ground), 0).unwrap();

    let mut ball_desc = ColliderDesc::ball(0.5);
    ball_desc.emit_collision_events = emit;
    let ball = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 3.0)), 1);
    world.add_collider(&ball_desc, Some(ball), 1).unwrap();

    let mut seen = Vec::new();
    for _ in 0..200 {
        world.step(1.0 / 60.0);
        seen.extend_from_slice(world.collision_events());
    }
    seen
}

#[test]
fn collision_events_are_off_by_default() {
    // 事件是有成本的，只给真正要监听的碰撞体打开。
    assert!(stack(false, false).is_empty());
}

#[test]
fn collision_events_arrive_when_enabled() {
    let events = stack(true, false);
    assert!(!events.is_empty(), "开了开关却没收到事件");
    assert!(events.iter().any(|e| e.started));
    assert!(events.iter().all(|e| !e.sensor));
}

#[test]
fn a_sensor_gets_events_without_opting_in() {
    // 传感器不开事件的话什么都不做，既不碰撞也不报告。
    // 所以它不受那个开关限制。
    let events = stack(false, true);
    assert!(!events.is_empty(), "传感器没自动开启事件");
    assert!(events.iter().any(|e| e.sensor && e.started));
}

#[test]
fn events_are_cleared_each_step() {
    // 不清的话事件会越攒越多，上层每帧都会重复处理同一次碰撞。
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(50.0, 0.5)).as_sensor(),
            Some(ground),
            0,
        )
        .unwrap();
    let ball = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 2.0)), 1);
    world.add_collider(&ColliderDesc::ball(0.5), Some(ball), 1).unwrap();

    // 跑到收到事件为止。
    let mut got = false;
    for _ in 0..200 {
        world.step(1.0 / 60.0);
        if !world.collision_events().is_empty() {
            got = true;
            break;
        }
    }
    assert!(got, "没收到事件，这条测试没意义");

    // 球已经穿过传感器了，再跑几步不该还带着上一次的事件。
    for _ in 0..120 {
        world.step(1.0 / 60.0);
    }
    assert!(
        world.collision_events().is_empty(),
        "事件没被清掉，上层会重复处理同一次碰撞"
    );
}

#[test]
fn both_colliders_appear_in_the_event() {
    let mut world = PhysicsWorld::new();
    let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
    let ground_collider = world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(50.0, 0.5)).as_sensor(),
            Some(ground),
            0,
        )
        .unwrap();
    let ball = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 3.0)), 1);
    let ball_collider = world.add_collider(&ColliderDesc::ball(0.5), Some(ball), 1).unwrap();

    for _ in 0..200 {
        world.step(1.0 / 60.0);
        if let Some(event) = world.collision_events().first() {
            let pair = [event.first, event.second];
            assert!(
                pair.contains(&ground_collider) && pair.contains(&ball_collider),
                "事件里的碰撞体对不上：{pair:?}"
            );
            return;
        }
    }
    panic!("没收到事件");
}
