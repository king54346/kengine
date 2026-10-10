//! 2D 关节在世界里真的起作用吗。
//!
//! `joint.rs` 里那些测试只验证「描述翻译成 rapier 的约束时锁对了轴」；
//! 这里验证的是**跑起来之后的行为**——摆锤真的吊着、绳子真的只在绷紧时
//! 拽人、限位真的挡住了反折。两层都要有：前者抓翻译错误，后者抓
//! 「翻译没错但接线接错了」。

use super::*;
use kmath::Vec2;

fn simulate(world: &mut PhysicsWorld, steps: usize) {
    for _ in 0..steps {
        world.step(1.0 / 60.0);
    }
}

/// 一个固定锚点 + 一个吊在 `offset` 处的小球。
fn anchor_and_bob(offset: Vec2) -> (PhysicsWorld, BodyHandle, BodyHandle) {
    let mut world = PhysicsWorld::new();
    let anchor = world.add_body(&RigidBodyDesc::fixed(), 0);
    let bob = world.add_body(&RigidBodyDesc::dynamic().with_position(offset), 1);
    world
        .add_collider(&ColliderDesc::ball(0.2), Some(bob), 1)
        .expect("摆锤该建得出来");
    (world, anchor, bob)
}

#[test]
fn a_hinge_keeps_its_body_at_a_fixed_distance() {
    // 铰链锁住位置、放开旋转。摆锤会荡下去，但离锚点的距离始终不变——
    // 这是「关节真的在约束」最直接的判据。
    let (mut world, anchor, bob) = anchor_and_bob(Vec2::new(2.0, 0.0));
    world.add_joint(
        anchor,
        bob,
        &JointDesc::revolute(Vec2::ZERO, Vec2::new(-2.0, 0.0), None),
    );

    simulate(&mut world, 180);

    let position = world.body(bob).expect("摆锤还在").position();
    let distance = position.length();

    assert!(
        (distance - 2.0).abs() < 0.15,
        "摆锤离锚点 {distance}，该一直是 2"
    );
    // 荡下去了才说明旋转确实是自由的。
    assert!(position.y < -0.5, "摆锤没荡下来，y = {}", position.y);
}

#[test]
fn a_fixed_joint_carries_its_body_along() {
    // 固定关节连位置带朝向都锁死，两块等于粘成一块。
    let (mut world, anchor, attached) = anchor_and_bob(Vec2::new(1.0, 0.0));
    world.add_joint(
        anchor,
        attached,
        &JointDesc::fixed(Vec2::new(1.0, 0.0), Vec2::ZERO),
    );

    simulate(&mut world, 180);

    let position = world.body(attached).expect("挂件还在").position();
    assert!(
        position.distance(Vec2::new(1.0, 0.0)) < 0.15,
        "固定关节没拉住，落到了 {position:?}"
    );
}

#[test]
fn a_rope_stops_the_fall_at_its_length() {
    // 绳索只限制最大距离：重物一路自由落体，到绳长处被拽住。
    let (mut world, anchor, weight) = anchor_and_bob(Vec2::ZERO);
    world.add_joint(
        anchor,
        weight,
        &JointDesc::rope(Vec2::ZERO, Vec2::ZERO, 3.0),
    );

    simulate(&mut world, 300);

    let distance = world.body(weight).expect("重物还在").position().length();

    assert!(distance <= 3.3, "绳子被拉长到了 {distance}，上限是 3");
    assert!(distance > 2.5, "重物该沉到把绳子绷直，现在只有 {distance}");
}

#[test]
fn a_rope_does_nothing_while_slack() {
    // 和铰链的根本区别：绳子松着的时候两端互不影响，重物该自由下落。
    let (mut world, anchor, weight) = anchor_and_bob(Vec2::ZERO);
    world.add_joint(
        anchor,
        weight,
        &JointDesc::rope(Vec2::ZERO, Vec2::ZERO, 100.0),
    );

    simulate(&mut world, 30);

    // 半秒自由落体约 1.2 米，远没到 100 米的绳长，所以不该被拽住。
    let y = world.body(weight).expect("重物还在").position().y;
    assert!(y < -0.8, "绳子松着却拽住了重物，y = {y}");
}

#[test]
fn hinge_limits_stop_the_swing() {
    // 布娃娃全靠限位：不给限位的话肘和膝会朝两边任意反折。
    let mut world = PhysicsWorld::new();
    let anchor = world.add_body(&RigidBodyDesc::fixed(), 0);
    let arm = world.add_body(
        &RigidBodyDesc::dynamic().with_position(Vec2::new(1.0, 0.0)),
        1,
    );
    world
        .add_collider(&ColliderDesc::cuboid(Vec2::new(0.5, 0.1)), Some(arm), 1)
        .expect("小臂");

    world.add_joint(
        anchor,
        arm,
        &JointDesc::revolute(Vec2::ZERO, Vec2::new(-1.0, 0.0), Some([-0.3, 0.3])),
    );

    simulate(&mut world, 240);

    let angle = world.body(arm).expect("小臂还在").rotation();
    assert!(
        angle.abs() < 0.5,
        "限位没起作用，转到了 {angle} 弧度（限位是 ±0.3）"
    );
}

#[test]
fn a_prismatic_joint_slides_along_one_axis_only() {
    // 滑轨：只能沿给定方向平移。给一条竖直轨道，重物该垂直下滑而不横移。
    let mut world = PhysicsWorld::new();
    let rail = world.add_body(&RigidBodyDesc::fixed(), 0);
    let slider = world.add_body(&RigidBodyDesc::dynamic(), 1);
    world
        .add_collider(&ColliderDesc::ball(0.2), Some(slider), 1)
        .expect("滑块");

    world.add_joint(
        rail,
        slider,
        &JointDesc::prismatic(Vec2::ZERO, Vec2::ZERO, Vec2::Y, Some([-2.0, 0.0])),
    );

    simulate(&mut world, 240);

    let position = world.body(slider).expect("滑块还在").position();
    assert!(position.x.abs() < 0.1, "滑块横向跑了 {}", position.x);
    assert!(
        position.y >= -2.2 && position.y < -1.5,
        "滑块该滑到行程下限附近，实际 y = {}",
        position.y
    );
}

#[test]
fn removing_a_joint_lets_the_body_go() {
    let (mut world, anchor, bob) = anchor_and_bob(Vec2::new(2.0, 0.0));
    let joint = world.add_joint(
        anchor,
        bob,
        &JointDesc::revolute(Vec2::ZERO, Vec2::new(-2.0, 0.0), None),
    );

    assert_eq!(world.joint_count(), 1);
    assert!(world.has_joint(joint));

    world.remove_joint(joint);

    assert_eq!(world.joint_count(), 0);
    assert!(!world.has_joint(joint));

    simulate(&mut world, 120);

    // 没了约束就是自由落体，两秒早掉出 2 米了。
    let y = world.body(bob).expect("摆锤还在").position().y;
    assert!(y < -2.0, "关节删了却还吊着，y = {y}");
}

#[test]
fn multibody_joints_form_a_chain_that_holds_together() {
    // 三节链：固定根 + 两节动态，铰链串起来。跑两秒，链节该还吊在原处，
    // 节距不变——这是「多体求解器真的在约束」最直接的判据。
    let mut world = PhysicsWorld::new();
    let root = world.add_body(
        &RigidBodyDesc::fixed().with_position(Vec2::new(0.0, 2.0)),
        0,
    );
    let mid = world.add_body(
        &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 1.0)),
        1,
    );
    let tip = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec2::ZERO), 2);
    for b in [mid, tip] {
        world
            .add_collider(&ColliderDesc::ball(0.2), Some(b), 0)
            .expect("链节该建得出来");
    }

    let j1 = world
        .add_multibody_joint(
            root,
            mid,
            &JointDesc::revolute(Vec2::new(0.0, -0.5), Vec2::new(0.0, 0.5), None),
        )
        .expect("root → mid 该成链");
    let j2 = world
        .add_multibody_joint(
            mid,
            tip,
            &JointDesc::revolute(Vec2::new(0.0, -0.5), Vec2::new(0.0, 0.5), None),
        )
        .expect("mid → tip 该成链");

    assert!(world.has_multibody_joint(j1));
    assert!(world.has_multibody_joint(j2));
    assert_eq!(world.multibody_joint_count(), 2);
    assert_eq!(world.joint_count(), 0);

    simulate(&mut world, 120);

    let mid_p = world.body(mid).expect("mid 还在").position();
    let tip_p = world.body(tip).expect("tip 还在").position();
    assert!(
        mid_p.distance(Vec2::new(0.0, 1.0)) < 0.3,
        "链节 mid 掉了：{mid_p:?}"
    );
    assert!(tip_p.distance(Vec2::ZERO) < 0.5, "链节 tip 掉了：{tip_p:?}");
    assert!(
        (mid_p.distance(tip_p) - 1.0).abs() < 0.15,
        "链节被拉长了：{mid_p:?} vs {tip_p:?}"
    );
}

#[test]
fn multibody_joints_reject_cycles() {
    let mut world = PhysicsWorld::new();
    let a = world.add_body(&RigidBodyDesc::dynamic(), 0);
    let b = world.add_body(&RigidBodyDesc::dynamic(), 1);
    let desc = || JointDesc::fixed(Vec2::ZERO, Vec2::ZERO);

    world
        .add_multibody_joint(a, b, &desc())
        .expect("a → b 该成链");
    assert!(
        world.add_multibody_joint(b, a, &desc()).is_none(),
        "成环的关节该返回 None"
    );
    assert_eq!(world.multibody_joint_count(), 1);
}

#[test]
fn removing_a_multibody_joint_frees_its_bodies() {
    let mut world = PhysicsWorld::new();
    let a = world.add_body(&RigidBodyDesc::dynamic(), 0);
    let b = world.add_body(&RigidBodyDesc::dynamic(), 1);
    let desc = JointDesc::fixed(Vec2::ZERO, Vec2::ZERO);

    let j = world
        .add_multibody_joint(a, b, &desc)
        .expect("a → b 该成链");
    world.remove_multibody_joint(j);
    assert!(!world.has_multibody_joint(j));
    assert_eq!(world.multibody_joint_count(), 0);
    assert!(
        world.add_multibody_joint(a, b, &desc).is_some(),
        "删掉之后该能重新成链"
    );
}

#[test]
fn removing_a_body_cleans_up_its_multibody_joints() {
    let mut world = PhysicsWorld::new();
    let a = world.add_body(&RigidBodyDesc::dynamic(), 0);
    let b = world.add_body(&RigidBodyDesc::dynamic(), 1);
    let j = world
        .add_multibody_joint(a, b, &JointDesc::fixed(Vec2::ZERO, Vec2::ZERO))
        .expect("a → b 该成链");

    world.remove_body(a);
    assert!(!world.has_multibody_joint(j));
    assert_eq!(world.multibody_joint_count(), 0);
}

#[test]
fn jointed_bodies_do_not_push_each_other_by_default() {
    // 连在一起的两块通常是重叠的（车轮陷在轮拱里）。默认开着碰撞的话
    // 它们会一直互相推，关节和碰撞打架，整个东西抖个不停。
    let mut world = PhysicsWorld::new();
    let a = world.add_body(&RigidBodyDesc::fixed(), 0);
    let b = world.add_body(&RigidBodyDesc::dynamic(), 1);
    world
        .add_collider(&ColliderDesc::ball(0.5), Some(a), 0)
        .expect("A");
    world
        .add_collider(&ColliderDesc::ball(0.5), Some(b), 1)
        .expect("B");

    // 两个球完全重叠，靠固定关节焊在一起。
    world.add_joint(a, b, &JointDesc::fixed(Vec2::ZERO, Vec2::ZERO));

    simulate(&mut world, 120);

    let position = world.body(b).expect("B 还在").position();
    assert!(
        position.length() < 0.2,
        "两个球互相弹开了，B 跑到了 {position:?}"
    );
}
