//! 空间查询：射线、形状扫掠、点投影。
//!
//! 这些查询走的是广相的加速结构，**不需要**步进物理世界，也不修改任何状态。
//! 拾取、瞄准、地面检测、视线判定都靠它们。

use crate::{BodyHandle, ColliderHandle, InteractionGroups};
use kmath::{Quat, Vec3};

/// 一次射线检测的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct RayCastOptions {
    /// 起点。
    pub origin: Vec3,
    /// 方向，不必是单位向量（内部会归一化）。
    pub direction: Vec3,
    /// 最大检测距离。
    pub max_distance: f32,
    /// 起点落在某个碰撞体**内部**时的行为。
    ///
    /// `true`：立刻命中，距离为 0；`false`：忽略这层壳，继续往前找出口面。
    /// 从角色胶囊内部往外打射线时，`false` 才能得到想要的结果。
    pub solid: bool,
    /// 过滤分组。
    pub groups: InteractionGroups,
    /// 忽略这个碰撞体。
    pub exclude_collider: Option<ColliderHandle>,
    /// 忽略这个刚体身上的所有碰撞体。射线从自己身上发出时必须排掉自己。
    pub exclude_body: Option<BodyHandle>,
}

impl Default for RayCastOptions {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            direction: Vec3::NEG_Z,
            max_distance: f32::MAX,
            solid: true,
            groups: InteractionGroups::ALL,
            exclude_collider: None,
            exclude_body: None,
        }
    }
}

impl RayCastOptions {
    /// 从 `origin` 朝 `direction` 打一条最长 `max_distance` 的射线。
    pub fn new(origin: Vec3, direction: Vec3, max_distance: f32) -> Self {
        Self {
            origin,
            direction,
            max_distance,
            ..Self::default()
        }
    }

    /// 限定过滤分组。
    pub fn with_groups(mut self, groups: InteractionGroups) -> Self {
        self.groups = groups;
        self
    }

    /// 排除某个刚体（连同它的所有碰撞体）。
    pub fn excluding_body(mut self, body: BodyHandle) -> Self {
        self.exclude_body = Some(body);
        self
    }

    /// 排除某个碰撞体。
    pub fn excluding_collider(mut self, collider: ColliderHandle) -> Self {
        self.exclude_collider = Some(collider);
        self
    }

    /// 设置 `solid`。
    pub fn with_solid(mut self, solid: bool) -> Self {
        self.solid = solid;
        self
    }
}

/// 射线命中的一个碰撞体。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// 被命中的碰撞体。
    pub collider: ColliderHandle,
    /// 该碰撞体所属的刚体，没有则为 `None`。
    pub body: Option<BodyHandle>,
    /// 碰撞体的用户数据。
    pub collider_user_data: u128,
    /// 所属刚体的用户数据；没有刚体时为 0。
    pub body_user_data: u128,
    /// 世界空间命中点。
    pub point: Vec3,
    /// 命中处的表面法线。射线起点在内部且 `solid` 为真时可能是零向量。
    pub normal: Vec3,
    /// 起点到命中点的距离。
    pub distance: f32,
}

/// 形状扫掠命中的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeHit {
    /// 被撞上的碰撞体。
    pub collider: ColliderHandle,
    /// 该碰撞体所属的刚体。
    pub body: Option<BodyHandle>,
    /// 碰撞体的用户数据。
    pub collider_user_data: u128,
    /// 所属刚体的用户数据；没有刚体时为 0。
    pub body_user_data: u128,
    /// 扫掠形状上的接触点（世界空间）。
    pub witness_on_shape: Vec3,
    /// 被撞碰撞体上的接触点（世界空间）。
    pub witness_on_collider: Vec3,
    /// 被撞碰撞体在接触处的法线。
    pub normal: Vec3,
    /// 沿扫掠方向走了多远撞上的。0 表示一开始就重叠。
    pub distance: f32,
}

/// 点投影的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointProjection {
    /// 最近的碰撞体。
    pub collider: ColliderHandle,
    /// 该碰撞体所属的刚体。
    pub body: Option<BodyHandle>,
    /// 碰撞体的用户数据。
    pub collider_user_data: u128,
    /// 所属刚体的用户数据；没有刚体时为 0。
    pub body_user_data: u128,
    /// 投影到表面上的点。
    pub point: Vec3,
    /// 查询点是否落在碰撞体内部。
    pub is_inside: bool,
}

/// 一次形状扫掠的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeCastOptions {
    /// 形状的起始位置。
    pub position: Vec3,
    /// 形状的起始朝向。
    pub rotation: Quat,
    /// 扫掠方向与速度。长度参与 `max_distance` 的换算。
    pub velocity: Vec3,
    /// 最大扫掠距离。
    pub max_distance: f32,
    /// 一开始就重叠时是否算命中。做「能不能站进去」的检测要设 `true`。
    pub stop_at_penetration: bool,
    /// 过滤分组。
    pub groups: InteractionGroups,
    /// 忽略这个刚体。
    pub exclude_body: Option<BodyHandle>,
}

impl Default for ShapeCastOptions {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            velocity: Vec3::NEG_Y,
            max_distance: 1.0,
            stop_at_penetration: true,
            groups: InteractionGroups::ALL,
            exclude_body: None,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn default_ray_is_unbounded_and_unfiltered() {
        let opts = RayCastOptions::default();

        assert_eq!(opts.max_distance, f32::MAX);
        assert_eq!(opts.groups, InteractionGroups::ALL);
        assert!(opts.exclude_body.is_none());
    }

    #[test]
    fn builders_compose() {
        let opts = RayCastOptions::new(Vec3::Y, Vec3::NEG_Y, 10.0)
            .with_groups(InteractionGroups::new(1, 1))
            .with_solid(false);

        assert_eq!(opts.origin, Vec3::Y);
        assert_eq!(opts.max_distance, 10.0);
        assert_eq!(opts.groups, InteractionGroups::new(1, 1));
        assert!(!opts.solid);
    }
}
