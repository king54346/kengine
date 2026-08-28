//! 2D 形状扫掠。
//!
//! 扫掠回答的是射线答不出的那个问题：射线是**一条线**，它说的是「这条线
//! 打到什么」；扫掠推的是**一整个形状**，说的是「这个东西整体挪过去会不会
//! 撞上」。子弹的连续检测和角色的贴墙滑动都非它不可，所以这里按那两个
//! 用途来测。

use super::*;
use kmath::Vec2;

/// 一堵竖在原点的墙：半宽 0.5，半高 5。
fn world_with_a_wall() -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    let wall = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(&ColliderDesc::cuboid(Vec2::new(0.5, 5.0)), Some(wall), 0)
        .expect("墙该建得出来");
    world
}

fn ball() -> ColliderShape {
    ColliderShape::Ball { radius: 0.25 }
}

#[test]
fn a_shape_cast_catches_what_frame_by_frame_tests_would_miss() {
    // 扫掠存在的全部理由：起点和终点都不重叠，中间却穿过去了。
    // 逐帧做重叠测试的话，这一帧在墙左、下一帧在墙右，两帧都「没碰上」，
    // 子弹就这么穿墙而过。
    let mut world = world_with_a_wall();

    let hit = world
        .cast_shape(
            &ball(),
            &ShapeCastOptions {
                position: Vec2::new(-10.0, 0.0),
                velocity: Vec2::X,
                max_distance: 20.0,
                ..Default::default()
            },
        )
        .expect("该撞上墙");

    // 球半径 0.25，墙左面在 x = -0.5，所以球心走到 -0.75 时贴上：10 − 0.75。
    assert!(
        (hit.distance - 9.25).abs() < 0.05,
        "撞上的距离是 {}",
        hit.distance
    );
    assert!(hit.normal.x < -0.9, "法线该朝左，实际是 {:?}", hit.normal);
}

#[test]
fn a_cast_that_falls_short_reports_nothing() {
    let mut world = world_with_a_wall();

    let hit = world.cast_shape(
        &ball(),
        &ShapeCastOptions {
            position: Vec2::new(-10.0, 0.0),
            velocity: Vec2::X,
            max_distance: 5.0,
            ..Default::default()
        },
    );

    assert!(hit.is_none(), "还没走到就不该报命中");
}

#[test]
fn a_cast_pointing_away_reports_nothing() {
    let mut world = world_with_a_wall();

    let hit = world.cast_shape(
        &ball(),
        &ShapeCastOptions {
            position: Vec2::new(-10.0, 0.0),
            velocity: Vec2::NEG_X,
            max_distance: 20.0,
            ..Default::default()
        },
    );

    assert!(hit.is_none(), "背对着墙也报了命中");
}

#[test]
fn a_cast_sees_a_freshly_added_collider() {
    // 查询结构在步进时才更新。忘了更新的话，刚加完碰撞体就扫会**静默扫空**
    // ——既不报错也不 panic，最难查的那种。这条盯着 `cast_shape` 里
    // 那句 `update_query_structures`。
    let mut world = world_with_a_wall();

    // 一步都没跑就直接查询。
    let hit = world.cast_shape(
        &ball(),
        &ShapeCastOptions {
            position: Vec2::new(-5.0, 0.0),
            velocity: Vec2::X,
            max_distance: 20.0,
            ..Default::default()
        },
    );

    assert!(hit.is_some(), "刚加的碰撞体没进查询结构");
}

#[test]
fn the_normal_is_what_wall_sliding_needs() {
    // 角色贴墙滑动的做法：扫一次拿法线，把速度里沿法线的那份减掉，
    // 剩下的就是沿墙的分量。这条测出那个减法确实得到了一个沿墙的方向——
    // 法线要是给错了，角色会卡在墙上不动，或者干脆穿进去。
    let mut world = world_with_a_wall();

    let velocity = Vec2::new(1.0, 0.5).normalize();
    let hit = world
        .cast_shape(
            &ball(),
            &ShapeCastOptions {
                position: Vec2::new(-5.0, 0.0),
                velocity,
                max_distance: 20.0,
                ..Default::default()
            },
        )
        .expect("该撞上");

    let slide = velocity - hit.normal * velocity.dot(hit.normal);

    assert!(slide.y.abs() > 0.3, "滑动分量该保留纵向速度：{slide:?}");
    assert!(slide.x.abs() < 0.1, "滑动分量不该还朝着墙里去：{slide:?}");
}

#[test]
fn an_overlapping_start_reports_zero_distance() {
    // 起点就埋在墙里：`stop_at_penetration` 开着时报「距离 0 的命中」。
    // 做「这个位置站不站得下」的检测必须开着，否则已经卡在墙里的角色会
    // 得到「前方无阻挡」，然后一头扎得更深。
    let mut world = world_with_a_wall();

    let hit = world
        .cast_shape(
            &ball(),
            &ShapeCastOptions {
                position: Vec2::ZERO,
                velocity: Vec2::X,
                max_distance: 5.0,
                stop_at_penetration: true,
                ..Default::default()
            },
        )
        .expect("重叠时该报命中");

    assert_eq!(hit.distance, 0.0, "一开始就重叠，距离该是 0");
}

#[test]
fn collision_groups_filter_the_cast() {
    // 扫掠要能挑目标：子弹不该被自己的枪管挡住。
    let mut world = PhysicsWorld::new();
    let wall = world.add_body(&RigidBodyDesc::fixed(), 0);
    world
        .add_collider(
            &ColliderDesc::cuboid(Vec2::new(0.5, 5.0))
                .with_collision_groups(crate::InteractionGroups::new(0b10, 0b10)),
            Some(wall),
            0,
        )
        .expect("墙");

    let options = |groups| ShapeCastOptions {
        position: Vec2::new(-5.0, 0.0),
        velocity: Vec2::X,
        max_distance: 20.0,
        groups,
        ..Default::default()
    };

    assert!(
        world
            .cast_shape(&ball(), &options(crate::InteractionGroups::new(0b10, 0b10)))
            .is_some(),
        "同组的该撞上"
    );
    assert!(
        world
            .cast_shape(&ball(), &options(crate::InteractionGroups::new(0b01, 0b01)))
            .is_none(),
        "不同组的不该撞上"
    );
}

#[test]
fn a_bigger_shape_hits_sooner() {
    // 扫的是形状不是点：半径越大，越早碰到墙。这条把「形状真的参与了计算」
    // 钉死——只拿中心点去扫的话，两个半径会得到同一个距离。
    let mut world = world_with_a_wall();

    let cast = |world: &mut PhysicsWorld, radius: f32| {
        world
            .cast_shape(
                &ColliderShape::Ball { radius },
                &ShapeCastOptions {
                    position: Vec2::new(-10.0, 0.0),
                    velocity: Vec2::X,
                    max_distance: 20.0,
                    ..Default::default()
                },
            )
            .expect("该撞上")
            .distance
    };

    let small = cast(&mut world, 0.25);
    let large = cast(&mut world, 1.0);

    assert!(
        large < small - 0.5,
        "大球该更早撞上：小球 {small}，大球 {large}"
    );
}
