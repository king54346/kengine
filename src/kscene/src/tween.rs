//! 挂在节点上的补间：位置、旋转、缩放在给定时间里过渡到目标值。
//!
//! ```
//! use kscene::{Node, Scene, TweenProperty};
//! use kanim::Ease;
//! use kmath::Vec3;
//!
//! let mut scene = Scene::new();
//! let door = scene.add_node(Node::new("Door"));
//! let id = scene.tween_position(door, Vec3::new(0.0, 3.0, 0.0), 0.5, Ease::OutCubic);
//! for _ in 0..40 {
//!     scene.tick_animations(1.0 / 60.0);
//! }
//! assert!(!scene.tween_active(id));
//! assert_eq!(scene[door].transform.position, Vec3::new(0.0, 3.0, 0.0));
//! ```
//!
//! # 规矩
//!
//! - **起点取开始那一刻的值**，不是创建场景时的值。
//! - **同一节点同一属性只留最新的那个**：连点两次「开门」，第二次从门当前的位置
//!   接着走，不会和第一次抢着写。
//! - 节点删掉了，它的补间跟着作废。
//! - 推进放在 [`Scene::tick_animations`] 里，和骨骼动画同一时刻：都写局部变换，
//!   都得排在世界变换重算之前。

use crate::{Node, Scene};
use kanim::{Ease, Tween};
use kcore::pool::Handle;
use kmath::{Quat, Vec3};

/// 补间改哪个属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TweenProperty {
    /// 局部位置。
    Position,
    /// 局部旋转（球面插值，走最短弧）。
    Rotation,
    /// 局部缩放。
    Scale,
}

/// 一段补间的编号。0 不会被分配，可以拿来当「没有」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TweenId(pub u64);

#[derive(Debug, Clone)]
enum Track {
    Vector(Tween<Vec3>),
    Rotation(Tween<Quat>),
}

#[derive(Debug, Clone)]
pub(crate) struct NodeTween {
    id: TweenId,
    node: Handle<Node>,
    property: TweenProperty,
    track: Track,
}

/// 场景里所有在跑的补间。
#[derive(Debug, Clone, Default)]
pub(crate) struct Tweens {
    active: Vec<NodeTween>,
    next_id: u64,
    /// 最近走完的几段（不是被顶替或取消的）。脚本的 `await` 靠它分辨两种结局。
    finished: std::collections::VecDeque<TweenId>,
}

/// 记住最近多少段走完的补间。
const FINISHED_MEMORY: usize = 256;

impl Scene {
    /// 让节点的局部位置在 `duration` 秒内过渡到 `to`。节点无效时返回 `TweenId(0)`。
    pub fn tween_position(
        &mut self,
        node: Handle<Node>,
        to: Vec3,
        duration: f32,
        ease: Ease,
    ) -> TweenId {
        let Some(from) = self.try_get(node).map(|n| n.transform.position) else {
            return TweenId(0);
        };
        self.start_tween(
            node,
            TweenProperty::Position,
            Track::Vector(Tween::new(from, to, duration, ease)),
        )
    }

    /// 让节点的局部缩放在 `duration` 秒内过渡到 `to`。
    pub fn tween_scale(
        &mut self,
        node: Handle<Node>,
        to: Vec3,
        duration: f32,
        ease: Ease,
    ) -> TweenId {
        let Some(from) = self.try_get(node).map(|n| n.transform.scale) else {
            return TweenId(0);
        };
        self.start_tween(
            node,
            TweenProperty::Scale,
            Track::Vector(Tween::new(from, to, duration, ease)),
        )
    }

    /// 让节点的局部旋转在 `duration` 秒内过渡到 `to`。
    pub fn tween_rotation(
        &mut self,
        node: Handle<Node>,
        to: Quat,
        duration: f32,
        ease: Ease,
    ) -> TweenId {
        let Some(from) = self.try_get(node).map(|n| n.transform.rotation) else {
            return TweenId(0);
        };
        self.start_tween(
            node,
            TweenProperty::Rotation,
            Track::Rotation(Tween::new(from, to, duration, ease)),
        )
    }

    /// 这段补间还在跑吗。走完、被顶替、被取消、节点被删，都算不在跑了。
    pub fn tween_active(&self, id: TweenId) -> bool {
        self.tweens.active.iter().any(|t| t.id == id)
    }

    /// 这段补间是不是**走完了**（而不是被顶替、取消或节点被删）。只记得最近 256 段。
    pub fn tween_finished(&self, id: TweenId) -> bool {
        self.tweens.finished.contains(&id)
    }

    /// 停掉一段补间，属性停在当前值。
    pub fn cancel_tween(&mut self, id: TweenId) {
        self.tweens.active.retain(|t| t.id != id);
    }

    /// 停掉一个节点上的所有补间。
    pub fn cancel_tweens_of(&mut self, node: Handle<Node>) {
        self.tweens.active.retain(|t| t.node != node);
    }

    fn start_tween(
        &mut self,
        node: Handle<Node>,
        property: TweenProperty,
        track: Track,
    ) -> TweenId {
        self.tweens.next_id += 1;
        let id = TweenId(self.tweens.next_id);
        self.tweens
            .active
            .retain(|t| !(t.node == node && t.property == property));
        self.tweens.active.push(NodeTween {
            id,
            node,
            property,
            track,
        });
        id
    }

    /// 推进所有补间并写回局部变换。由 [`Scene::tick_animations`] 调用。
    pub(crate) fn tick_tweens(&mut self, dt: f32) {
        if self.tweens.active.is_empty() {
            return;
        }
        let mut active = std::mem::take(&mut self.tweens.active);
        let finished_log = &mut self.tweens.finished;
        active.retain_mut(|tween| {
            let Ok(node) = self.nodes.try_borrow_mut(tween.node) else {
                return false;
            };
            let finished = match &mut tween.track {
                Track::Vector(track) => {
                    let value = track.advance(dt);
                    match tween.property {
                        TweenProperty::Scale => node.transform.scale = value,
                        _ => node.transform.position = value,
                    }
                    track.finished()
                }
                Track::Rotation(track) => {
                    node.transform.rotation = track.advance(dt).normalize();
                    track.finished()
                }
            };
            if finished {
                if finished_log.len() >= FINISHED_MEMORY {
                    finished_log.pop_front();
                }
                finished_log.push_back(tween.id);
            }
            !finished
        });
        // 推进期间不会有新补间进来（这里不跑用户代码），直接放回去。
        self.tweens.active = active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_tween_on_the_same_property_replaces_the_old_one() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("n"));
        let first = scene.tween_position(node, Vec3::new(10.0, 0.0, 0.0), 1.0, Ease::Linear);
        scene.tick_animations(0.5);
        assert_eq!(scene[node].transform.position, Vec3::new(5.0, 0.0, 0.0));
        // 第二段从当前位置（5）接着走，第一段作废。
        let second = scene.tween_position(node, Vec3::ZERO, 1.0, Ease::Linear);
        assert!(!scene.tween_active(first));
        assert!(!scene.tween_finished(first), "被顶替不算走完");
        scene.tick_animations(0.5);
        assert_eq!(scene[node].transform.position, Vec3::new(2.5, 0.0, 0.0));
        // 不同属性互不影响。
        let grow = scene.tween_scale(node, Vec3::splat(2.0), 1.0, Ease::Linear);
        assert!(scene.tween_active(second) && scene.tween_active(grow));
    }

    #[test]
    fn rotation_takes_the_short_way_and_lands_exactly() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("n"));
        let target = Quat::from_rotation_y(1.5);
        let id = scene.tween_rotation(node, target, 0.2, Ease::InOutSine);
        for _ in 0..20 {
            scene.tick_animations(0.05);
        }
        assert!(!scene.tween_active(id));
        assert!(scene.tween_finished(id));
        assert!(scene[node].transform.rotation.angle_between(target) < 1e-4);
    }

    #[test]
    fn removing_the_node_drops_its_tweens() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("n"));
        let id = scene.tween_position(node, Vec3::ONE, 1.0, Ease::Linear);
        scene.remove_node(node);
        scene.tick_animations(0.1);
        assert!(!scene.tween_active(id));
        assert_eq!(
            scene.tween_position(node, Vec3::ONE, 1.0, Ease::Linear),
            TweenId(0)
        );
    }

    #[test]
    fn cancelling_leaves_the_value_where_it_was() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("n"));
        let id = scene.tween_position(node, Vec3::new(0.0, 4.0, 0.0), 1.0, Ease::Linear);
        scene.tick_animations(0.25);
        scene.cancel_tween(id);
        scene.tick_animations(0.25);
        assert_eq!(scene[node].transform.position, Vec3::new(0.0, 1.0, 0.0));
    }
}
