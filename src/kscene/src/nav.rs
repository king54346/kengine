//! 导航接到场景上：用物理射线和地形烘 [`knav::NavGrid`]。
//!
//! ```no_run
//! # use kscene::Scene;
//! # use kmath::Vec3;
//! # let mut scene = Scene::new();
//! // 物理刚体建出来之后（至少步进过一次）烘一次：
//! let grid = scene.bake_nav_grid(Vec3::new(-20.0, -1.0, -20.0), Vec3::new(20.0, 10.0, 20.0), Default::default());
//! let path = grid.find_path(Vec3::new(-5.0, 0.0, 0.0), Vec3::new(8.0, 0.0, 3.0));
//! ```
//!
//! 只认**静态**的东西：没有刚体的碰撞体、固定刚体、地形。动态刚体（箱子）和运动学刚体
//! （角色自己）不算——前者会被推走，后者就是要寻路的那个。会动的障碍用 `NavGrid::block` 临时挡。

use kcore::pool::Handle;
use kmath::{Vec2, Vec3};
use kphysics::{RayCastOptions, RigidBodyType};

use crate::{Node, Scene};

impl Scene {
    /// 在 `min..max` 这个盒子里烘一张导航格子：每格从 `max.y` 往下打一条射线，打到的第一个静态表面
    /// （或地形）就是那格的地面。打穿到 `min.y` 都没碰到的格子没有地面。
    pub fn bake_nav_grid(
        &self,
        min: Vec3,
        max: Vec3,
        settings: knav::NavGridSettings,
    ) -> knav::NavGrid {
        let mut hits = Vec::new();
        let drop = (max.y - min.y).max(0.0);
        knav::NavGrid::bake(
            Vec2::new(min.x, min.z),
            Vec2::new(max.x, max.z),
            settings,
            |p| {
                let origin = Vec3::new(p.x, max.y, p.y);
                let options = RayCastOptions {
                    origin,
                    direction: Vec3::NEG_Y,
                    max_distance: drop,
                    solid: true,
                    ..Default::default()
                };
                self.physics.cast_ray_all(&options, &mut hits);
                let mut best = hits
                    .iter()
                    .filter(|hit| self.is_static_body(hit.body_user_data, hit.body.is_some()))
                    .map(|hit| (hit.distance, hit.normal))
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                if let Some((_, world)) = self.raycast_terrain(origin, Vec3::NEG_Y, drop) {
                    let distance = origin.y - world.y;
                    if best.is_none_or(|(d, _)| distance < d) {
                        // 地形法线用旁边两个采样差出来（射线接口不给法线）。
                        let e = settings.cell_size * 0.5;
                        let sample = |dx: f32, dz: f32| {
                            let o = origin + Vec3::new(dx, 0.0, dz);
                            self.raycast_terrain(o, Vec3::NEG_Y, drop)
                                .map_or(world.y, |(_, w)| w.y)
                        };
                        let normal = Vec3::new(
                            sample(-e, 0.0) - sample(e, 0.0),
                            2.0 * e,
                            sample(0.0, -e) - sample(0.0, e),
                        )
                        .normalize();
                        best = Some((distance, normal));
                    }
                }
                best.map(|(distance, normal)| knav::GroundSample {
                    height: origin.y - distance,
                    normal,
                })
            },
        )
    }

    /// 射线打到的刚体算不算静态：没有刚体、或者固定刚体。
    fn is_static_body(&self, body_user_data: u128, has_body: bool) -> bool {
        if !has_body {
            return true;
        }
        let node = Handle::<Node>::decode_from_u128(body_user_data);
        self.try_get(node)
            .and_then(Node::rigid_body)
            .is_none_or(|body| body.body_type() == RigidBodyType::Fixed)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Collider, Node, RigidBody, Scene};
    use kmath::Vec3;

    #[test]
    fn baking_sees_static_walls_but_not_the_character_or_crates() {
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.05, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(10.0, 0.05, 10.0))),
        );
        // x = 0 一道 2 米高的墙，z 在 -6..6。
        scene.add_node(
            Node::new("wall")
                .with_position(Vec3::new(0.0, 1.0, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(0.2, 1.0, 6.0))),
        );
        // 角色站在起点上，一个动态箱子挡在半路——都不该进导航。
        scene.add_node(
            Node::new("player")
                .with_position(Vec3::new(-5.0, 1.0, 0.0))
                .with_rigid_body(RigidBody::kinematic())
                .with_collider(Collider::capsule_y(0.5, 0.3)),
        );
        scene.add_node(
            Node::new("crate")
                .with_position(Vec3::new(-3.0, 0.5, -7.0))
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::cuboid(Vec3::splat(0.5))),
        );
        scene.step_physics(1.0 / 60.0);
        scene.update();

        let grid = scene.bake_nav_grid(
            Vec3::new(-10.0, -1.0, -10.0),
            Vec3::new(10.0, 5.0, 10.0),
            Default::default(),
        );
        assert!(
            grid.walkable_at(Vec3::new(-5.0, 0.0, 0.0)),
            "角色自己不该被烘成障碍"
        );
        assert!(
            !grid.walkable_at(Vec3::new(0.0, 2.0, 0.0)),
            "墙顶太窄，站不住"
        );
        assert!(
            !grid.walkable_at(Vec3::new(-0.4, 0.0, 0.0)),
            "墙脚离墙不到代理半径"
        );
        let path = grid
            .find_path(Vec3::new(-5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 0.0))
            .expect("绕过墙头");
        assert!(path.iter().any(|p| p.z.abs() > 6.0), "{path:?}");
        assert!(
            path.iter().all(|p| p.y.abs() < 0.01),
            "一直在地面上：{path:?}"
        );
    }
}
