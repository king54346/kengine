//! 角色控制器：让一个角色**好好走路**。
//!
//! # 为什么不能用刚体硬凑
//!
//! 把主角做成动态刚体、靠加力推着走，四件事永远调不对：
//!
//! - **爬不上台阶**。一级 20 厘米的台阶会把胶囊卡住，因为求解器看到的是
//!   一个法线朝侧面的接触，它只会把角色往外推。
//! - **斜坡上会滑下去**。静摩擦不是用来"站住"的，它只是让滑动变慢。
//! - **撞墙会打转或弹开**，而不是贴着墙滑过去。
//! - **跳跃手感僵**。速度是求解器算出来的，不是你给的，想做二段跳、
//!   土狼时间、可变跳跃高度都得跟求解器较劲。
//!
//! 正确的做法是**运动学**的：每帧自己算出想走多远，交给一个专门的算法
//! 去做"扫掠 → 碰到东西 → 沿表面滑 → 再扫掠"的迭代，最后拿到一个
//! 实际能走的位移。这就是 [`CharacterController`]。
//!
//! # 重力要自己加
//!
//! 控制器**不管重力**——它只回答"想走这么远，实际能走多远"。
//! 竖直方向的速度要你自己积分：
//!
//! ```
//! # use kphysics::*;
//! # use kmath::Vec3;
//! # let mut world = PhysicsWorld::new();
//! # let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
//! # world.add_collider(&ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)), Some(ground), 0);
//! # let body = world.add_body(&RigidBodyDesc::kinematic_position_based()
//! #     .with_position(Vec3::new(0.0, 3.0, 0.0)), 1);
//! # world.add_collider(&ColliderDesc::capsule_y(0.5, 0.3), Some(body), 1);
//! let controller = CharacterController::default();
//! let mut vertical = 0.0_f32;
//! let dt = 1.0 / 60.0;
//!
//! for _ in 0..120 {
//!     vertical += -9.81 * dt;
//!     let desired = Vec3::new(0.0, vertical * dt, 0.0);
//!     let movement = world.move_character(&controller, body, desired, dt);
//!
//!     // 落地了就把下坠速度清掉，否则会越积越大，
//!     // 下次离开地面的瞬间角色会像被弹弓射出去。
//!     if movement.grounded {
//!         vertical = 0.0;
//!     }
//! }
//! assert!(world.body(body).unwrap().position().y < 1.5);
//! ```

use crate::{
    BodyHandle, ColliderHandle, InteractionGroups,
    convert::{from_rv, to_rv},
};
use kmath::Vec3;

/// 一段长度：要么是绝对值，要么是相对角色高度的比例。
///
/// 用比例的好处是换个角色尺寸不用重调参数——一个两米高的角色和一个
/// 半米高的角色，"能迈过自己身高四分之一的台阶"是同一句话。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// 绝对长度（米）。
    Absolute(f32),
    /// 相对角色形状尺寸的比例。
    Relative(f32),
}

impl Length {
    fn to_rapier(self) -> rapier3d::control::CharacterLength {
        match self {
            Length::Absolute(value) => rapier3d::control::CharacterLength::Absolute(value),
            Length::Relative(value) => rapier3d::control::CharacterLength::Relative(value),
        }
    }
}

/// 自动上台阶的设置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Autostep {
    /// 最高能迈过多高的台阶。
    pub max_height: Length,
    /// 迈上去之后前方至少要有多宽的空地。
    ///
    /// 防的是"迈上一个窄台子然后立刻卡住"——没有这个判据的话，
    /// 角色会爬上一个站不住的地方然后原地抖动。
    pub min_width: Length,
    /// 能不能踩着动态物体往上迈。
    pub include_dynamic_bodies: bool,
}

impl Default for Autostep {
    fn default() -> Self {
        Self {
            max_height: Length::Relative(0.25),
            min_width: Length::Relative(0.5),
            include_dynamic_bodies: true,
        }
    }
}

/// 角色控制器的参数。
///
/// 这是一份**纯配置**，不持有任何状态——角色的位置在刚体上，速度归你自己管。
/// 于是同一个控制器可以驱动很多角色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterController {
    /// 哪个方向算"上"。地板的判定、坡度的计算都以它为基准。
    pub up: Vec3,
    /// 角色和周围保持的一点缝隙。
    ///
    /// **不能为零**：贴合到零距离时，浮点误差会让下一帧的扫掠从
    /// "已经穿透"的状态开始，算出来的法线是乱的。也不能太大，
    /// 不然角色看上去浮在墙外面。
    pub offset: Length,
    /// 撞到东西时是否沿表面滑过去。
    ///
    /// 关掉的话撞墙就是硬停——做冰球、弹球这类东西才需要。
    pub slide: bool,
    /// 自动上台阶。为 [`None`] 时关闭。
    ///
    /// 默认**开着**，和 rapier 的默认相反：rapier 关掉它是因为开销不小，
    /// 但一个上不了台阶的角色控制器基本没法用，这个开销该默认付。
    pub autostep: Option<Autostep>,
    /// 能爬多陡的坡（弧度，地面法线与 `up` 的夹角）。
    pub max_slope_climb_angle: f32,
    /// 陡到多少度开始自动往下滑（弧度）。
    ///
    /// 必须**小于等于** `max_slope_climb_angle`，否则会出现一段
    /// "既爬不上去也不往下滑"的角度，角色卡在坡上不动。
    pub min_slope_slide_angle: f32,
    /// 离地多近时自动吸附到地面。为 [`None`] 时关闭。
    ///
    /// 不开的话下坡会变成一连串小跳——角色沿斜面水平走出去，
    /// 离地了，下一帧靠重力落回来，如此往复。
    pub snap_to_ground: Option<Length>,
    /// 碰撞过滤：只和这些组交互。
    pub groups: InteractionGroups,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            up: Vec3::Y,
            // 相对值：换个角色尺寸不用重调。
            offset: Length::Relative(0.01),
            slide: true,
            autostep: Some(Autostep::default()),
            // 45°：比这更陡的坡爬不上去。
            max_slope_climb_angle: std::f32::consts::FRAC_PI_4,
            // 30°：比这更陡就开始下滑。留出 15° 的"能站住但爬不上去"区间。
            min_slope_slide_angle: std::f32::consts::FRAC_PI_6,
            snap_to_ground: Some(Length::Relative(0.2)),
            groups: InteractionGroups::ALL,
        }
    }
}

impl CharacterController {
    /// 关掉自动上台阶。
    pub fn without_autostep(mut self) -> Self {
        self.autostep = None;
        self
    }

    /// 关掉沿墙滑动。
    pub fn without_sliding(mut self) -> Self {
        self.slide = false;
        self
    }

    /// 设置能爬的最大坡度（弧度）。
    ///
    /// 会同时把下滑角度夹到不超过它——两者反过来的话会出现一段
    /// 「既爬不上去也不往下滑」的角度，角色卡在坡上不动。
    pub fn with_max_slope(mut self, radians: f32) -> Self {
        self.max_slope_climb_angle = radians;
        self.min_slope_slide_angle = self.min_slope_slide_angle.min(radians);
        self
    }

    /// 设置碰撞过滤组。
    pub fn with_groups(mut self, groups: InteractionGroups) -> Self {
        self.groups = groups;
        self
    }

    pub(crate) fn to_rapier(self) -> rapier3d::control::KinematicCharacterController {
        rapier3d::control::KinematicCharacterController {
            up: to_rv(self.up.normalize_or(Vec3::Y)),
            offset: self.offset.to_rapier(),
            slide: self.slide,
            autostep: self.autostep.map(|step| rapier3d::control::CharacterAutostep {
                max_height: step.max_height.to_rapier(),
                min_width: step.min_width.to_rapier(),
                include_dynamic_bodies: step.include_dynamic_bodies,
            }),
            max_slope_climb_angle: self.max_slope_climb_angle,
            // 夹一下：下滑角度大于爬升角度时，中间那段角色会卡住不动。
            min_slope_slide_angle: self.min_slope_slide_angle.min(self.max_slope_climb_angle),
            snap_to_ground: self.snap_to_ground.map(Length::to_rapier),
            normal_nudge_factor: 1.0e-4,
        }
    }
}

/// 一次移动的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterMovement {
    /// **实际**发生的位移。可能比想走的短（撞墙了），
    /// 也可能方向不同（沿墙滑了）。
    pub translation: Vec3,
    /// 移动之后是否踩在地上。
    ///
    /// 跳跃、下坠动画、脚步声都看它。注意它是**移动之后**的状态，
    /// 所以刚起跳的那一帧它已经是 `false` 了。
    pub grounded: bool,
    /// 是否正因为坡太陡而往下滑。
    pub sliding_down_slope: bool,
}

/// 角色撞到的一个东西。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterCollision {
    /// 撞到的碰撞体。
    pub collider: ColliderHandle,
    /// 撞到的刚体，对方没有刚体时为 [`None`]。
    pub body: Option<BodyHandle>,
    /// 接触点（世界空间）。
    pub point: Vec3,
    /// 接触法线（世界空间），指向角色这一侧。
    pub normal: Vec3,
    /// 撞上时角色已经走了多远。
    pub distance: f32,
}

impl crate::PhysicsWorld {
    /// 移动一个角色，返回实际发生的位移。
    ///
    /// `body` 应当是**运动学**刚体（[`RigidBodyDesc::kinematic_position_based`]），
    /// 而且要挂着碰撞体——角色的形状就是那个碰撞体。挂多个的话用第一个。
    ///
    /// 算完之后会**直接把刚体挪过去**（走 `set_next_kinematic_position`，
    /// 于是沿途能正确推开动态物体）。只想算不想动的话用
    /// [`compute_character_movement`](Self::compute_character_movement)。
    ///
    /// # 重力要自己加
    ///
    /// 见模块文档。控制器只回答「想走这么远，实际能走多远」。
    ///
    /// # 刚体不存在或没有碰撞体时
    ///
    /// 返回一个零位移、`grounded` 为假的结果，什么都不做。
    pub fn move_character(
        &mut self,
        controller: &CharacterController,
        body: BodyHandle,
        desired: Vec3,
        dt: f32,
    ) -> CharacterMovement {
        let movement = self.compute_character_movement(controller, body, desired, dt, &mut |_| {});

        if movement.translation != Vec3::ZERO
            && let Some(mut handle) = self.body_mut(body)
        {
            let position = handle.position() + movement.translation;
            let rotation = handle.rotation();
            handle.set_next_kinematic_position(position, rotation);
        }
        movement
    }

    /// 同上，但**只算不动**，并且把沿途撞到的东西报给回调。
    ///
    /// 用来做「撞到什么就播什么音效」「推箱子」这类事。位移要自己应用。
    pub fn compute_character_movement(
        &mut self,
        controller: &CharacterController,
        body: BodyHandle,
        desired: Vec3,
        dt: f32,
        on_collision: &mut dyn FnMut(CharacterCollision),
    ) -> CharacterMovement {
        let none = CharacterMovement {
            translation: Vec3::ZERO,
            grounded: false,
            sliding_down_slope: false,
        };

        // 查询走的是广相的 BVH，而 BVH 是在 `step` 里维护的。刚加完
        // 角色就移动的话，查询树里还没有那些墙——角色会直接穿过去。
        self.update_query_structures();

        let Some(rigid_body) = self.inner.bodies.get(body.0) else {
            return none;
        };
        let Some(collider_handle) = rigid_body.colliders().first().copied() else {
            // 没有碰撞体就没有形状，无从扫掠。返回零位移而不是让它
            // 自由穿墙——后者更难查。
            // 没日志依赖，用 debug_assert 把这个错误在开发期喊出来。
            // 发布版里安静返回零位移——总比让角色自由穿墙好查。
            debug_assert!(false, "角色刚体没有碰撞体，move_character 什么都不会做");
            return none;
        };
        let Some(collider) = self.inner.colliders.get(collider_handle) else {
            return none;
        };

        let shape = collider.shared_shape().clone();
        let pose = *collider.position();
        let native = controller.to_rapier();

        // 过滤掉角色自己：不排除的话第一次扫掠就会撞上自己的碰撞体，
        // 结果是角色一步也走不动。
        let filter = rapier3d::prelude::QueryFilter::new()
            .groups(controller.groups.to_rapier())
            .exclude_rigid_body(body.0)
            .exclude_collider(collider_handle);

        let mut collisions = Vec::new();
        let result = native.move_shape(
            dt,
            &self.inner.query_pipeline_with_filter(filter),
            &*shape,
            &pose,
            to_rv(desired),
            |collision| collisions.push(collision),
        );

        // 事件在扫掠结束之后再报：回调里可能会去查场景，而此刻
        // `move_shape` 还借着查询管线。
        for collision in collisions {
            let body = self
                .inner
                .colliders
                .get(collision.handle)
                .and_then(|c| c.parent())
                .map(BodyHandle);
            on_collision(CharacterCollision {
                collider: ColliderHandle(collision.handle),
                body,
                point: from_rv(collision.hit.witness1.into()),
                normal: from_rv(collision.hit.normal1.into()),
                distance: collision.hit.time_of_impact,
            });
        }

        CharacterMovement {
            translation: from_rv(result.translation),
            grounded: result.grounded,
            sliding_down_slope: result.is_sliding_down_slope,
        }
    }
}
