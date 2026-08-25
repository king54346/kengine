//! 阴影逐级剔除的分批测试。
//!
//! 关键约束：实例下标是**全局的**、和主 pass 共用一个数组，所以剔除
//! 只能把批次切成若干段，**不能重排也不能压缩**。压缩了的话下标就错位，
//! 阴影会画在别的物体的位置上。

use super::*;
use kmath::{Aabb, Vec3};

fn batch_of(first: u32, count: u32) -> Batch {
    Batch {
        mesh_id: Uuid::from_u128(1),
        shader_id: Uuid::nil(),
        texture_key: [Uuid::from_u128(1); 5],
        skinned: false,
        first,
        count,
    }
}

fn box_at(x: f32) -> Aabb {
    Aabb::from_center_half_extents(Vec3::new(x, 0.0, 0.0), Vec3::splat(1.0))
}

/// 一个覆盖 [-10, 10]² 的正交光矩阵。
fn matrix() -> Mat4 {
    Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 0.0, 20.0)
        * Mat4::look_at_rh(Vec3::new(0.0, 10.0, 0.0), Vec3::ZERO, Vec3::Z)
}

#[test]
fn everything_visible_keeps_one_batch() {
    let bounds = vec![box_at(0.0), box_at(1.0), box_at(2.0)];
    let out = cascade_batches(&[batch_of(0, 3)], &bounds, matrix(), 1024, 0.0);

    assert_eq!(out.len(), 1, "全可见却被切开了");
    assert_eq!((out[0].first, out[0].count), (0, 3));
}

#[test]
fn everything_culled_yields_nothing() {
    let bounds = vec![box_at(500.0), box_at(600.0)];
    let out = cascade_batches(&[batch_of(0, 2)], &bounds, matrix(), 1024, 0.0);
    assert!(out.is_empty());
}

#[test]
fn a_hole_in_the_middle_splits_the_batch() {
    // 中间那个被剔掉，两边各成一段。合成一段的话中间那个会被画上，
    // 剔除等于白做。
    let bounds = vec![box_at(0.0), box_at(500.0), box_at(2.0)];
    let out = cascade_batches(&[batch_of(0, 3)], &bounds, matrix(), 1024, 0.0);

    assert_eq!(out.len(), 2);
    assert_eq!((out[0].first, out[0].count), (0, 1));
    assert_eq!((out[1].first, out[1].count), (2, 1));
}

#[test]
fn instance_indices_are_preserved_not_compacted() {
    // 这是整个设计的核心约束。压缩下标的话，阴影会用错的模型矩阵——
    // 影子出现在别的物体的位置上。
    let bounds = vec![box_at(500.0), box_at(500.0), box_at(0.0)];
    let out = cascade_batches(&[batch_of(0, 3)], &bounds, matrix(), 1024, 0.0);

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].first, 2, "下标被压缩了，该是 2 不是 0");
}

#[test]
fn a_batch_that_does_not_start_at_zero_is_handled() {
    // 半透明批次接在不透明后面，first 不是 0。
    let bounds = vec![
        box_at(500.0),
        box_at(500.0),
        box_at(0.0),
        box_at(500.0),
        box_at(1.0),
    ];
    let out = cascade_batches(&[batch_of(2, 3)], &bounds, matrix(), 1024, 0.0);

    assert_eq!(out.len(), 2);
    assert_eq!((out[0].first, out[0].count), (2, 1));
    assert_eq!((out[1].first, out[1].count), (4, 1));
}

#[test]
fn batch_metadata_is_carried_over() {
    // 切出来的段必须保留网格 / 贴图 / 蒙皮标记，否则会用错管线或错网格。
    let mut source = batch_of(0, 2);
    source.skinned = true;
    source.mesh_id = Uuid::from_u128(42);
    let bounds = vec![box_at(0.0), box_at(1.0)];

    let out = cascade_batches(&[source], &bounds, matrix(), 1024, 0.0);
    assert_eq!(out[0].mesh_id, Uuid::from_u128(42));
    assert!(out[0].skinned);
}

#[test]
fn several_batches_are_processed_independently() {
    let bounds = vec![box_at(0.0), box_at(500.0), box_at(1.0), box_at(2.0)];
    let out = cascade_batches(
        &[batch_of(0, 2), batch_of(2, 2)],
        &bounds,
        matrix(),
        1024,
        0.0,
    );

    assert_eq!(out.len(), 2);
    assert_eq!((out[0].first, out[0].count), (0, 1));
    assert_eq!((out[1].first, out[1].count), (2, 2));
}

#[test]
fn a_batch_running_past_the_bounds_array_does_not_panic() {
    // 下标对不上是个 bug，但崩掉整帧比少画一个影子糟得多。
    let bounds = vec![box_at(0.0)];
    let out = cascade_batches(&[batch_of(0, 10)], &bounds, matrix(), 1024, 0.0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].count, 1);
}

#[test]
fn size_culling_removes_small_objects() {
    let tiny = Aabb::from_center_half_extents(Vec3::ZERO, Vec3::splat(0.001));
    let bounds = vec![tiny, box_at(0.0)];

    let without = cascade_batches(&[batch_of(0, 2)], &bounds, matrix(), 1024, 0.0);
    assert_eq!(without[0].count, 2, "关掉尺寸剔除时两个都该画");

    let with = cascade_batches(&[batch_of(0, 2)], &bounds, matrix(), 1024, 2.0);
    assert_eq!(with.len(), 1);
    assert_eq!((with[0].first, with[0].count), (1, 1), "小物件没被剔掉");
}

#[test]
fn culling_cuts_the_work_on_a_spread_out_scene() {
    // 端到端：一片撒开的物体，一级只该留下附近那些。
    // 这一条不成立说明剔除装了但没生效。
    let bounds: Vec<Aabb> = (0..200).map(|i| box_at(i as f32 * 2.0 - 200.0)).collect();
    let full = batch_of(0, bounds.len() as u32);

    let out = cascade_batches(&[full], &bounds, matrix(), 1024, 0.0);
    let kept: u32 = out.iter().map(|b| b.count).sum();

    assert!(
        kept < bounds.len() as u32 / 4,
        "留下了 {kept}/{}，剔除没生效",
        bounds.len()
    );
    assert!(kept > 0, "一个都没留下，把该画的也剔了");
}
