//! 碰撞事件。
//!
//! 事件不是免费的：只有显式开了
//! [`ColliderDesc::emit_collision_events`](crate::ColliderDesc) 的碰撞体才会上报。
//! 默认全关，是因为绝大多数碰撞体（地面、道具、碎石）根本没人监听。

use crate::ColliderHandle;
use kmath::Vec3;

/// 两个碰撞体开始或结束接触。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollisionEvent {
    /// 其中一个碰撞体。
    pub collider1: ColliderHandle,
    /// 另一个碰撞体。
    pub collider2: ColliderHandle,
    /// `collider1` 的用户数据；碰撞体已被删除时为 0。
    pub user_data1: u128,
    /// `collider2` 的用户数据；碰撞体已被删除时为 0。
    pub user_data2: u128,
    /// `true` 是开始接触，`false` 是结束接触。
    pub started: bool,
    /// 这对里至少有一个是传感器。传感器的接触不产生任何物理响应。
    pub sensor: bool,
    /// 接触结束的原因是碰撞体被删掉了，而不是真的分开了。
    ///
    /// 这种事件里两边的用户数据往往已经查不到，别拿它去反查场景节点。
    pub removed: bool,
}

impl CollisionEvent {
    /// 这次碰撞是否涉及某个用户数据。
    pub fn involves_user_data(&self, user_data: u128) -> bool {
        self.user_data1 == user_data || self.user_data2 == user_data
    }

    /// 给定其中一边的用户数据，返回**另一边**的。不涉及该值时返回 `None`。
    ///
    /// 写「我撞到了谁」这类逻辑时省掉一次分支判断——事件里两个碰撞体的
    /// 先后顺序是引擎定的，不能假设自己一定是 `collider1`。
    pub fn other_user_data(&self, mine: u128) -> Option<u128> {
        if self.user_data1 == mine {
            Some(self.user_data2)
        } else if self.user_data2 == mine {
            Some(self.user_data1)
        } else {
            None
        }
    }

    /// 给定其中一边的碰撞体，返回另一边的。
    pub fn other_collider(&self, mine: ColliderHandle) -> Option<ColliderHandle> {
        if self.collider1 == mine {
            Some(self.collider2)
        } else if self.collider2 == mine {
            Some(self.collider1)
        } else {
            None
        }
    }
}

/// 一对碰撞体之间的接触力。
///
/// 只有设了接触力阈值的碰撞体才会上报。用来做「撞得够狠才掉血 / 才碎」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactForceEvent {
    /// 其中一个碰撞体。
    pub collider1: ColliderHandle,
    /// 另一个碰撞体。
    pub collider2: ColliderHandle,
    /// `collider1` 的用户数据。
    pub user_data1: u128,
    /// `collider2` 的用户数据。
    pub user_data2: u128,
    /// 所有接触点上的力的矢量和。
    pub total_force: Vec3,
    /// 各接触点力的**大小之和**。
    ///
    /// 与 `total_force.length()` 不同：方向相反的两个力会在矢量和里抵消，
    /// 但在这里仍然累加。判断「撞得有多狠」应该用这个。
    pub total_force_magnitude: f32,
    /// 最大的那个接触力的方向。
    pub max_force_direction: Vec3,
    /// 最大的那个接触力的大小。
    pub max_force_magnitude: f32,
}

#[cfg(test)]
mod test {
    use super::*;
    use rapier3d::geometry::ColliderHandle as RapierColliderHandle;

    fn event(user_data1: u128, user_data2: u128) -> CollisionEvent {
        CollisionEvent {
            collider1: ColliderHandle(RapierColliderHandle::from_raw_parts(0, 0)),
            collider2: ColliderHandle(RapierColliderHandle::from_raw_parts(1, 0)),
            user_data1,
            user_data2,
            started: true,
            sensor: false,
            removed: false,
        }
    }

    #[test]
    fn other_user_data_works_from_either_side() {
        // 引擎不保证谁排在前面，两个方向都得对。
        let e = event(10, 20);

        assert_eq!(e.other_user_data(10), Some(20));
        assert_eq!(e.other_user_data(20), Some(10));
        assert_eq!(e.other_user_data(30), None);
    }

    #[test]
    fn involves_user_data_matches_either_side() {
        let e = event(10, 20);

        assert!(e.involves_user_data(10));
        assert!(e.involves_user_data(20));
        assert!(!e.involves_user_data(30));
    }

    #[test]
    fn other_collider_works_from_either_side() {
        let e = event(10, 20);

        assert_eq!(e.other_collider(e.collider1), Some(e.collider2));
        assert_eq!(e.other_collider(e.collider2), Some(e.collider1));
    }
}
