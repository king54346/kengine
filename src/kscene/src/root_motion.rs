//! 根运动接到场景上：动画里根骨骼走的那段路，交给节点（或者角色控制器）去走。
//!
//! `kanim` 那一半（[`kanim::RootMotion`]）把根骨骼的位移从姿态里抽出来、按帧给增量；这里补上
//! 「它是哪根骨头」「增量在世界里是多少」「谁来走」：
//!
//! ```no_run
//! # use kscene::{Scene, Node};
//! # let mut scene = Scene::new();
//! # let model = scene.root();
//! scene.enable_root_motion(model, "mixamorig:Hips", false);
//! // 每帧，tick_animations 之后：
//! # let dt = 1.0 / 60.0;
//! scene.tick_animations(dt);
//! scene.apply_root_motion(model); // 没有物理：直接挪模型
//! // 有角色控制器时换成：
//! // let step = scene.root_motion_delta(model);
//! // scene.move_character(body, &controller, step + gravity * dt, dt);
//! ```
//!
//! 只抽平移；根骨骼的转向（转身动画）留在姿态里。

use kcore::pool::Handle;
use kmath::Vec3;

use crate::{Node, Scene};

impl Scene {
    /// `model` 子树里第一个动画播放器（模型实例化时挂在模型根或它下面）。
    fn animator_holder(&self, model: Handle<Node>) -> Option<Handle<Node>> {
        std::iter::once(model)
            .chain(self.descendants(model))
            .find(|&handle| {
                self.try_get(handle)
                    .is_some_and(|node| node.animator().is_some())
            })
    }

    /// 打开 `model` 的根运动：名叫 `bone` 的节点当根骨骼。`vertical` 为真时竖直位移也抽出来
    /// （跳跃一类），否则上下起伏留在动画里。
    ///
    /// 找不到动画播放器、或者播放器驱动的节点里没有这个名字时返回 `false`。
    pub fn enable_root_motion(&mut self, model: Handle<Node>, bone: &str, vertical: bool) -> bool {
        let Some(holder) = self.animator_holder(model) else {
            return false;
        };
        let Some(player) = self.try_get(holder).and_then(Node::animator) else {
            return false;
        };
        let target = (0..player.target_count()).find(|&index| {
            self.try_get(player.target(index))
                .is_some_and(|node| node.name == bone)
        });
        let Some(target) = target else { return false };
        if let Some(player) = self.try_get_mut(holder).and_then(Node::animator_mut) {
            player
                .animator_mut()
                .set_root_motion(Some(kanim::RootMotion { target, vertical }));
            return true;
        }
        false
    }

    /// 关掉 `model` 的根运动（根骨骼重新照动画走）。
    pub fn disable_root_motion(&mut self, model: Handle<Node>) {
        if let Some(holder) = self.animator_holder(model)
            && let Some(player) = self.try_get_mut(holder).and_then(Node::animator_mut)
        {
            player.animator_mut().set_root_motion(None);
        }
    }

    /// 上一次 [`tick_animations`](Self::tick_animations) 里，根骨骼在**世界空间**里走了多远。
    ///
    /// `kanim` 给的增量在根骨骼父节点的空间里；这里乘上父节点的世界变换（含模型的缩放——
    /// Mixamo 模型常常缩到 0.01，动画里走 100 个单位就是世界里 1 米）。没开根运动时是 0。
    pub fn root_motion_delta(&self, model: Handle<Node>) -> Vec3 {
        let Some(holder) = self.animator_holder(model) else {
            return Vec3::ZERO;
        };
        let Some(player) = self.try_get(holder).and_then(Node::animator) else {
            return Vec3::ZERO;
        };
        let Some(root_motion) = player.animator().root_motion() else {
            return Vec3::ZERO;
        };
        let delta = player.animator().root_motion_delta();
        let bone = player.target(root_motion.target);
        let parent = self.try_get(bone).map_or(Handle::NONE, |node| node.parent);
        if parent.is_none() {
            delta
        } else {
            self.world_matrix(parent).transform_vector3(delta)
        }
    }

    /// 把根运动的位移直接加到 `model` 节点上（没有物理的场合；有角色控制器时用
    /// [`root_motion_delta`](Self::root_motion_delta) 喂给 [`move_character`](Self::move_character)）。
    /// 返回走了的世界位移。
    pub fn apply_root_motion(&mut self, model: Handle<Node>) -> Vec3 {
        let delta = self.root_motion_delta(model);
        if delta == Vec3::ZERO {
            return delta;
        }
        let parent = self.try_get(model).map_or(Handle::NONE, |node| node.parent);
        let local = if parent.is_none() {
            delta
        } else {
            self.world_matrix(parent).inverse().transform_vector3(delta)
        };
        if let Some(node) = self.try_get_mut(model) {
            node.transform.position += local;
        }
        delta
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnimationPlayer, Node, Scene};
    use kanim::{AnimationClip, Animator, Channel, Curve, Interpolation, Track};
    use kmath::Vec3;
    use std::sync::Arc;

    /// 模型（缩放 2）→ 骨架 → 髋骨；剪辑里髋骨一秒往 +Z 走 2 个单位。
    fn walker() -> (Scene, kcore::pool::Handle<Node>, kcore::pool::Handle<Node>) {
        let mut scene = Scene::new();
        let model = scene.add_node(Node::new("Model").with_scale(Vec3::splat(2.0)));
        let armature = scene.add_node_with_parent(Node::new("Armature"), model);
        let hips = scene.add_node_with_parent(Node::new("Hips"), armature);
        let walk = AnimationClip::new(
            "walk",
            vec![Track {
                target: 0,
                channel: Channel::Position(
                    Curve::new(
                        vec![0.0, 1.0],
                        vec![Vec3::ZERO, Vec3::new(0.0, 0.0, 2.0)],
                        Interpolation::Linear,
                    )
                    .unwrap(),
                ),
            }],
        );
        let mut animator = Animator::new(Arc::new(vec![walk]));
        animator.play(0);
        let player = AnimationPlayer::new(animator, vec![hips]);
        if let Some(node) = scene.try_get_mut(model) {
            *node = std::mem::replace(node, Node::new("tmp")).with_animator(player);
        }
        scene.update();
        (scene, model, hips)
    }

    #[test]
    fn root_motion_moves_the_model_and_keeps_the_hips_in_place() {
        let (mut scene, model, hips) = walker();
        assert!(scene.enable_root_motion(model, "Hips", false));
        let mut travelled = Vec3::ZERO;
        for _ in 0..30 {
            scene.tick_animations(1.0 / 60.0);
            travelled += scene.apply_root_motion(model);
            scene.update();
        }
        // 半秒：动画里走 1 个单位，模型缩放 2 → 世界里 2 米。
        assert!((travelled.z - 2.0).abs() < 1e-3, "走了 {travelled}");
        assert!(
            (scene[model].transform.position.z - 2.0).abs() < 1e-3,
            "模型在 {}",
            scene[model].transform.position
        );
        assert!(
            scene[hips].transform.position.z.abs() < 1e-5,
            "髋骨应该留在原地：{}",
            scene[hips].transform.position
        );
    }

    #[test]
    fn without_root_motion_nothing_is_handed_out() {
        let (mut scene, model, hips) = walker();
        scene.tick_animations(0.5);
        assert_eq!(scene.root_motion_delta(model), Vec3::ZERO);
        assert!((scene[hips].transform.position.z - 1.0).abs() < 1e-3);
        assert!(!scene.enable_root_motion(model, "NoSuchBone", false));
    }
}
