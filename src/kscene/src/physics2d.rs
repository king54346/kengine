//! 场景里的 2D 物理。
//!
//! # 为什么不像 3D 那样做成节点组件
//!
//! 3D 物理是组件式的：节点上挂 `RigidBody` / `Collider`，[`Scene::update`]
//! 负责建原生刚体、双向同步、处理增删。那套机制是为**场景图**设计的——
//! 3D 物体有父子关系、有蒙皮、有布娃娃，必须跟着树走。
//!
//! 2D 不一样。这个引擎的 2D 走的是**立即模式精灵**（[`Scene::push_sprite`]）——
//! 每帧声明画什么，不建节点。给它套一层节点组件等于强迫 2D 游戏
//! 去用一个它根本不需要的场景图。
//!
//! 所以这里的做法是：**物理世界直接挂在场景上**，游戏自己读刚体位置
//! 去发精灵。想用节点的话有 [`Scene::sync_node_from_2d_body`]。
//!
//! ```no_run
//! # use kscene::Scene;
//! # use kphysics::d2::{RigidBodyDesc, ColliderDesc};
//! # use kmath::Vec2;
//! # let mut scene = Scene::new();
//! let body = scene.physics2d_mut().add_body(&RigidBodyDesc::dynamic(), 0);
//! scene.physics2d_mut().add_collider(&ColliderDesc::ball(0.5), Some(body), 0);
//!
//! // 每帧：
//! scene.step_physics_2d(1.0 / 60.0);
//! let position = scene.physics2d().body(body).unwrap().position();
//! // ...拿 position 去发精灵
//! ```

use crate::{Node, Scene};
use kcore::pool::Handle;
use kmath::{Quat, Vec3};
use kphysics::d2;

impl Scene {
    /// 场景的 2D 物理世界。
    ///
    /// 和 [`Scene::physics`] 是两个**独立**的世界，互不感知。
    /// 一个 2D 刚体永远不会撞到一个 3D 刚体。
    pub fn physics2d(&self) -> &d2::PhysicsWorld {
        &self.physics2d
    }

    /// 可写地拿到 2D 物理世界。
    pub fn physics2d_mut(&mut self) -> &mut d2::PhysicsWorld {
        &mut self.physics2d
    }

    /// 步进 2D 物理。
    ///
    /// **和 [`Scene::step_physics`] 是分开的**：绝大多数场景只用其中一个，
    /// 合在一起的话 3D 游戏每帧都要白跑一次空的 2D 世界。
    pub fn step_physics_2d(&mut self, dt: f32) {
        self.physics2d.step(dt);
    }

    /// 把一个 2D 刚体的位姿写到节点上。
    ///
    /// 映射关系：刚体的 `(x, y)` → 节点位置的 `(x, y)`（**Z 保持不动**），
    /// 刚体的角度 → 绕 **Z 轴**的旋转。这是 2D 世界躺在 XY 平面上、
    /// 相机沿 -Z 看过去的标准摆法。
    ///
    /// Z 不动是有意的：2D 游戏常用 Z 做图层排序，物理不该踩掉它。
    ///
    /// 刚体或节点不在了就什么都不做，返回 `false`。
    pub fn sync_node_from_2d_body(&mut self, node: Handle<Node>, body: d2::BodyHandle) -> bool {
        let Some((position, rotation)) = self
            .physics2d
            .body(body)
            .map(|body| (body.position(), body.rotation()))
        else {
            return false;
        };
        let Some(node) = self.try_get_mut(node) else {
            return false;
        };

        node.transform.position = Vec3::new(position.x, position.y, node.transform.position.z);
        node.transform.rotation = Quat::from_rotation_z(rotation);
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kmath::Vec2;
    use kphysics::d2::{ColliderDesc, RigidBodyDesc};

    #[test]
    fn a_fresh_scene_has_an_empty_2d_world() {
        let scene = Scene::new();
        assert_eq!(scene.physics2d().body_count(), 0);
    }

    #[test]
    fn the_2d_world_steps_independently_of_the_3d_one() {
        // 合在一起的话 3D 游戏每帧都要白跑一次空的 2D 世界。
        let mut scene = Scene::new();
        let body = scene.physics2d_mut().add_body(
            &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 10.0)),
            0,
        );
        scene
            .physics2d_mut()
            .add_collider(&ColliderDesc::ball(0.5), Some(body), 0);

        // 只步进 3D，2D 不该动。
        scene.step_physics(1.0 / 60.0);
        assert_eq!(
            scene.physics2d().body(body).unwrap().position(),
            Vec2::new(0.0, 10.0)
        );

        // 步进 2D，该往下掉。
        for _ in 0..60 {
            scene.step_physics_2d(1.0 / 60.0);
        }
        assert!(scene.physics2d().body(body).unwrap().position().y < 9.0);
    }

    #[test]
    fn syncing_writes_x_and_y_to_the_node() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Sprite"));
        let body = scene.physics2d_mut().add_body(
            &RigidBodyDesc::fixed().with_position(Vec2::new(3.0, 4.0)),
            0,
        );

        assert!(scene.sync_node_from_2d_body(node, body));
        let transform = &scene.try_get(node).unwrap().transform;
        assert_eq!(transform.position.x, 3.0);
        assert_eq!(transform.position.y, 4.0);
    }

    #[test]
    fn syncing_leaves_z_alone() {
        // 2D 游戏常用 Z 做图层排序，物理不该踩掉它。
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Sprite"));
        scene.try_get_mut(node).unwrap().transform.position = Vec3::new(0.0, 0.0, -7.5);

        let body = scene.physics2d_mut().add_body(
            &RigidBodyDesc::fixed().with_position(Vec2::new(3.0, 4.0)),
            0,
        );
        scene.sync_node_from_2d_body(node, body);

        assert_eq!(scene.try_get(node).unwrap().transform.position.z, -7.5);
    }

    #[test]
    fn syncing_maps_the_angle_to_a_z_rotation() {
        // 2D 世界躺在 XY 平面上，相机沿 -Z 看过去。绕别的轴转的话
        // 精灵会翻到侧面去，看上去像凭空消失。
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Sprite"));
        let body = scene
            .physics2d_mut()
            .add_body(&RigidBodyDesc::fixed().with_rotation(0.5), 0);
        scene.sync_node_from_2d_body(node, body);

        let rotation = scene.try_get(node).unwrap().transform.rotation;
        // 绕 Z 转 0.5 弧度：X 轴该转到 (cos, sin, 0)。
        let turned = rotation * Vec3::X;
        assert!((turned.x - 0.5_f32.cos()).abs() < 1e-4, "{turned:?}");
        assert!((turned.y - 0.5_f32.sin()).abs() < 1e-4, "{turned:?}");
        assert!(turned.z.abs() < 1e-5, "转到平面外去了：{turned:?}");
    }

    #[test]
    fn syncing_a_missing_body_or_node_does_nothing() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Sprite"));
        let body = scene.physics2d_mut().add_body(&RigidBodyDesc::fixed(), 0);

        scene.physics2d_mut().remove_body(body);
        assert!(!scene.sync_node_from_2d_body(node, body));

        let live = scene.physics2d_mut().add_body(&RigidBodyDesc::fixed(), 0);
        scene.remove_node(node);
        assert!(!scene.sync_node_from_2d_body(node, live));
    }

    #[test]
    fn simulation_results_reach_the_node() {
        // 端到端：掉下来的球要真的把节点带下去。
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Ball"));

        let ground = scene.physics2d_mut().add_body(&RigidBodyDesc::fixed(), 0);
        scene.physics2d_mut().add_collider(
            &ColliderDesc::cuboid(Vec2::new(50.0, 0.5)),
            Some(ground),
            0,
        );
        let ball = scene.physics2d_mut().add_body(
            &RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 10.0)),
            1,
        );
        scene
            .physics2d_mut()
            .add_collider(&ColliderDesc::ball(0.5), Some(ball), 1);

        for _ in 0..180 {
            scene.step_physics_2d(1.0 / 60.0);
            scene.sync_node_from_2d_body(node, ball);
        }

        let y = scene.try_get(node).unwrap().transform.position.y;
        assert!((y - 1.0).abs() < 0.15, "节点该跟到 y≈1.0，实测 {y}");
    }
}
