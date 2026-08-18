//! 粒子碰撞。
//!
//! # 两级，因为代价差了三个数量级
//!
//! - **平面碰撞**：一组无限大平面，逐粒子一次点积。四千个粒子配三块平面
//!   也就是一万两千次点积，随并行推进一起做掉，成本可以忽略。
//!   地面、房间的四壁、桌面——绝大多数需求就是这些。
//! - **场景碰撞**：对真实的物理世界打射线。准确，但**很贵**：
//!   逐粒子每帧一次射线，四千个粒子就是每秒二十四万次查询。
//!   所以它按预算轮转——每帧只处理一小批，靠若干帧覆盖全部粒子。
//!
//! 这不是偷懒，是这类效果的通行做法：粒子碰撞在视觉上只要「大致对」，
//! 一颗火花晚两帧才弹起来没人看得出，而每帧全量射线检测会直接吃掉帧预算。
//!
//! # 分层
//!
//! 本 crate **不依赖物理引擎**。平面碰撞是纯数学；场景碰撞由调用方传一个
//! 射线检测闭包进来（见 [`ParticleSystem::resolve_scene_collisions`]），
//! `kscene` 用 `kphysics` 实现它。于是 kparticle 仍然能在没有物理、
//! 没有 GPU 的环境里完整测试。

use kmath::{Plane, Vec3};

/// 撞上表面之后怎么办。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionResponse {
    /// 弹性：法向速度保留多少。0 = 贴住不弹，1 = 原速弹回。
    pub restitution: f32,
    /// 摩擦：切向速度每次碰撞损失多少。0 = 无摩擦地滑走，1 = 切向速度归零。
    pub friction: f32,
    /// 每次碰撞额外消耗的寿命比例（相对总寿命）。
    ///
    /// 让弹了几次的火花先熄灭，比让它们一直弹到寿终自然。
    pub lifetime_loss: f32,
    /// 碰撞即死。做「打到墙上就没了」的火花、溅到地上就消失的水滴。
    pub kill: bool,
    /// 粒子的碰撞半径。0 表示当质点处理。
    ///
    /// 与视觉尺寸解耦：烟雾的方片可以很大，但碰撞上当成一个点才不会
    /// 老远就被墙挡住。
    pub radius: f32,
}

impl Default for CollisionResponse {
    fn default() -> Self {
        Self {
            restitution: 0.3,
            friction: 0.2,
            lifetime_loss: 0.0,
            kill: false,
            radius: 0.0,
        }
    }
}

impl CollisionResponse {
    /// 弹性碰撞：弹起来，几乎不损失能量。
    pub fn bouncy() -> Self {
        Self {
            restitution: 0.8,
            friction: 0.05,
            ..Self::default()
        }
    }

    /// 撞上就停：法向不弹、切向也几乎不滑。做「粘在墙上」的效果。
    pub fn sticky() -> Self {
        Self {
            restitution: 0.0,
            friction: 1.0,
            ..Self::default()
        }
    }

    /// 撞上就消失。
    pub fn kill_on_impact() -> Self {
        Self {
            kill: true,
            ..Self::default()
        }
    }

    /// 指定弹性与摩擦。
    pub fn with_material(mut self, restitution: f32, friction: f32) -> Self {
        self.restitution = restitution.clamp(0.0, 1.0);
        self.friction = friction.clamp(0.0, 1.0);
        self
    }

    /// 指定每次碰撞消耗的寿命比例。
    pub fn with_lifetime_loss(mut self, loss: f32) -> Self {
        self.lifetime_loss = loss.max(0.0);
        self
    }

    /// 指定碰撞半径。
    pub fn with_radius(mut self, radius: f32) -> Self {
        self.radius = radius.max(0.0);
        self
    }

    /// 按碰撞面的法线把速度反射掉。
    ///
    /// 只在**朝着**表面运动时才反射：已经在往外走的粒子再反射一次，
    /// 就会被按回表面里，看起来像贴着地面抖。
    pub fn reflect(&self, velocity: Vec3, normal: Vec3) -> Vec3 {
        let along_normal = velocity.dot(normal);
        if along_normal >= 0.0 {
            return velocity;
        }
        let normal_part = normal * along_normal;
        let tangent_part = velocity - normal_part;
        -normal_part * self.restitution + tangent_part * (1.0 - self.friction)
    }
}

/// 粒子系统的碰撞设置。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Collision {
    /// 撞上之后怎么办。
    pub response: CollisionResponse,
    /// 一组无限大平面。法线指向粒子应当待的那一侧。
    pub planes: Vec<Plane>,
    /// 是否与物理世界的实际几何碰撞。
    ///
    /// 打开之后需要上层每帧调
    /// [`resolve_scene_collisions`](crate::ParticleSystem::resolve_scene_collisions)；
    /// `kscene` 会自动做这件事。
    pub scene: bool,
    /// 场景碰撞每帧最多检测几个粒子。
    ///
    /// 逐粒子每帧一次射线太贵，所以轮转覆盖：粒子多时，
    /// 一颗粒子平均每 `粒子数 / budget` 帧才被检测一次。
    pub scene_budget: usize,
}

impl Collision {
    /// 一块朝上的地面。
    pub fn ground(height: f32) -> Self {
        Self {
            planes: vec![Plane {
                normal: Vec3::Y,
                d: -height,
            }],
            ..Self::default()
        }
    }

    /// 只跟物理世界的实际几何碰撞。
    pub fn scene() -> Self {
        Self {
            scene: true,
            scene_budget: Self::DEFAULT_SCENE_BUDGET,
            ..Self::default()
        }
    }

    /// 场景碰撞的默认每帧预算。
    pub const DEFAULT_SCENE_BUDGET: usize = 64;

    /// 追加一块平面。
    pub fn with_plane(mut self, plane: Plane) -> Self {
        self.planes.push(plane);
        self
    }

    /// 指定碰撞响应。
    pub fn with_response(mut self, response: CollisionResponse) -> Self {
        self.response = response;
        self
    }

    /// 打开场景碰撞并指定每帧预算。
    pub fn with_scene(mut self, budget: usize) -> Self {
        self.scene = true;
        self.scene_budget = budget.max(1);
        self
    }

    /// 有没有任何一种碰撞是开着的。
    pub fn is_active(&self) -> bool {
        !self.planes.is_empty() || self.scene
    }
}

/// 一次碰撞的解算结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    /// 修正后的位置。
    pub position: Vec3,
    /// 修正后的速度。
    pub velocity: Vec3,
    /// 这次碰撞该扣掉的寿命（秒）。
    pub lifetime_cost: f32,
    /// 是否该让这颗粒子立即死亡。
    pub killed: bool,
}

/// 把一颗粒子推出表面并反射速度。
///
/// `point` 是表面上的接触点，`normal` 是表面法线。
pub fn resolve_at_surface(
    response: &CollisionResponse,
    point: Vec3,
    normal: Vec3,
    velocity: Vec3,
    lifetime: f32,
) -> Resolved {
    Resolved {
        // 沿法线抬出一个半径：正好贴在面上的话，下一帧的判定会在
        // 「里」「外」之间反复横跳。
        position: point + normal * response.radius,
        velocity: response.reflect(velocity, normal),
        lifetime_cost: lifetime * response.lifetime_loss,
        killed: response.kill,
    }
}

/// 逐平面判定并解算。没碰到任何平面时返回 [`None`]。
pub(crate) fn resolve_planes(
    collision: &Collision,
    position: Vec3,
    velocity: Vec3,
    lifetime: f32,
) -> Option<Resolved> {
    let response = &collision.response;
    let (mut position, mut velocity) = (position, velocity);
    let mut cost = 0.0;
    let mut hit_any = false;
    let mut killed = false;

    for plane in &collision.planes {
        let distance = plane.distance_to(position);
        if distance >= response.radius {
            continue;
        }
        // 平面是无限大的，「穿过去了」和「陷进去了」是同一件事，
        // 一个符号判定就够，不需要扫掠检测。
        let contact = position - plane.normal * distance;
        let resolved = resolve_at_surface(response, contact, plane.normal, velocity, lifetime);

        position = resolved.position;
        velocity = resolved.velocity;
        cost += resolved.lifetime_cost;
        killed |= resolved.killed;
        hit_any = true;
    }

    hit_any.then_some(Resolved {
        position,
        velocity,
        lifetime_cost: cost,
        killed,
    })
}

/// 场景碰撞查询到的表面。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceHit {
    /// 世界空间接触点。
    pub point: Vec3,
    /// 接触处的表面法线。
    pub normal: Vec3,
}

#[cfg(test)]
mod test {
    use super::*;

    fn ground() -> Plane {
        Plane {
            normal: Vec3::Y,
            d: 0.0,
        }
    }

    #[test]
    fn a_particle_above_the_plane_is_left_alone() {
        let collision = Collision::default().with_plane(ground());

        assert!(resolve_planes(&collision, Vec3::Y * 5.0, Vec3::NEG_Y, 1.0).is_none());
    }

    #[test]
    fn a_particle_below_the_plane_is_pushed_back_out() {
        let collision = Collision::default().with_plane(ground());
        let resolved = resolve_planes(&collision, Vec3::new(1.0, -2.0, 3.0), Vec3::NEG_Y, 1.0)
            .expect("陷进地面了却没判定成碰撞");

        assert_eq!(resolved.position, Vec3::new(1.0, 0.0, 3.0));
        // 只在竖直方向修正，水平位置不该被动。
        assert_eq!(resolved.position.x, 1.0);
        assert_eq!(resolved.position.z, 3.0);
    }

    #[test]
    fn the_radius_keeps_the_particle_off_the_surface() {
        // 正好贴在面上的话，下一帧的判定会在「里」「外」之间反复横跳。
        let collision = Collision::default()
            .with_plane(ground())
            .with_response(CollisionResponse::default().with_radius(0.25));

        let resolved = resolve_planes(&collision, Vec3::ZERO, Vec3::NEG_Y, 1.0).unwrap();

        assert_eq!(resolved.position.y, 0.25);
    }

    #[test]
    fn restitution_controls_how_high_it_bounces() {
        let response = CollisionResponse::default().with_material(0.5, 0.0);

        let bounced = response.reflect(Vec3::new(0.0, -10.0, 0.0), Vec3::Y);

        assert_eq!(bounced, Vec3::new(0.0, 5.0, 0.0));
    }

    #[test]
    fn zero_restitution_stops_the_normal_motion_dead() {
        let response = CollisionResponse::sticky();
        let stopped = response.reflect(Vec3::new(3.0, -10.0, 0.0), Vec3::Y);

        assert_eq!(stopped, Vec3::ZERO, "完全粘住时切向也该归零");
    }

    #[test]
    fn friction_only_eats_the_tangential_part() {
        let response = CollisionResponse::default().with_material(1.0, 0.25);

        let after = response.reflect(Vec3::new(4.0, -2.0, 0.0), Vec3::Y);

        assert_eq!(after.y, 2.0, "法向被摩擦影响了");
        assert_eq!(after.x, 3.0, "切向该损失四分之一");
    }

    #[test]
    fn a_particle_already_moving_away_is_not_reflected_again() {
        // 再反射一次会把它按回表面里，看起来像贴着地面抖。
        let response = CollisionResponse::bouncy();
        let velocity = Vec3::new(1.0, 5.0, 0.0);

        assert_eq!(response.reflect(velocity, Vec3::Y), velocity);
    }

    #[test]
    fn several_planes_all_get_a_say() {
        // 墙角：两块平面同时把粒子往外推。
        let collision = Collision::default()
            .with_plane(ground())
            .with_plane(Plane {
                normal: Vec3::X,
                d: 0.0,
            });

        let resolved =
            resolve_planes(&collision, Vec3::new(-1.0, -1.0, 0.0), Vec3::new(-1.0, -1.0, 0.0), 1.0)
                .unwrap();

        assert!(resolved.position.x >= 0.0 && resolved.position.y >= 0.0, "没被推出墙角");
        assert!(resolved.velocity.x >= 0.0 && resolved.velocity.y >= 0.0);
    }

    #[test]
    fn a_non_axis_aligned_plane_reflects_correctly() {
        let slope = Plane {
            normal: Vec3::new(1.0, 1.0, 0.0).normalize(),
            d: 0.0,
        };
        let collision = Collision::default()
            .with_plane(slope)
            .with_response(CollisionResponse::default().with_material(1.0, 0.0));

        let resolved = resolve_planes(&collision, Vec3::new(-1.0, -1.0, 0.0), Vec3::NEG_Y, 1.0)
            .unwrap();

        // 完全弹性下速率不变，方向被镜像到斜面另一侧。
        assert!((resolved.velocity.length() - 1.0).abs() < 1e-5);
        assert!(resolved.velocity.dot(slope.normal) > 0.0, "反射后仍朝着斜面里");
        assert!(slope.distance_to(resolved.position) > -1e-5, "没被推到斜面外侧");
    }

    #[test]
    fn lifetime_loss_accumulates_per_plane() {
        let collision = Collision::default()
            .with_plane(ground())
            .with_plane(Plane {
                normal: Vec3::X,
                d: 0.0,
            })
            .with_response(CollisionResponse::default().with_lifetime_loss(0.25));

        let resolved =
            resolve_planes(&collision, Vec3::new(-1.0, -1.0, 0.0), Vec3::NEG_ONE, 4.0).unwrap();

        // 两块平面各扣 0.25 × 4 秒。
        assert_eq!(resolved.lifetime_cost, 2.0);
    }

    #[test]
    fn kill_on_impact_is_reported() {
        let collision = Collision::default()
            .with_plane(ground())
            .with_response(CollisionResponse::kill_on_impact());

        let resolved = resolve_planes(&collision, Vec3::NEG_Y, Vec3::NEG_Y, 1.0).unwrap();

        assert!(resolved.killed);
    }

    #[test]
    fn ground_helper_places_the_plane_at_the_given_height() {
        let collision = Collision::ground(3.0);

        assert_eq!(collision.planes[0].distance_to(Vec3::Y * 3.0), 0.0);
        assert!(collision.planes[0].distance_to(Vec3::Y * 5.0) > 0.0);
        assert!(collision.planes[0].distance_to(Vec3::ZERO) < 0.0);
    }

    #[test]
    fn an_empty_collision_setting_is_inactive() {
        // 没配任何平面、也没开场景碰撞时，推进循环应当整段跳过。
        assert!(!Collision::default().is_active());
        assert!(Collision::ground(0.0).is_active());
        assert!(Collision::scene().is_active());
    }

    #[test]
    fn material_parameters_are_clamped_to_sane_ranges() {
        // 弹性大于 1 会让粒子每次碰撞获得能量，越弹越高直到飞出场景。
        let response = CollisionResponse::default().with_material(5.0, -3.0);

        assert_eq!(response.restitution, 1.0);
        assert_eq!(response.friction, 0.0);
    }
}
