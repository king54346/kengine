//! 2D 角色控制器。
//!
//! 和 3D 那套（[`crate::CharacterController`]）是同一个算法、同一套参数，
//! 只是向量换成 [`Vec2`]。平台跳跃游戏的手感几乎全在这上面：
//! 能不能上台阶、贴不贴墙滑、斜坡站不站得住。
//!
//! # 重力要自己加
//!
//! 控制器只回答「想走这么远，实际能走多远」。竖直速度归你自己积分——
//! 二段跳、土狼时间、可变跳跃高度都因此变成几行普通代码。

use super::{
    convert::{from_rv, to_rv},
    world::{BodyHandle, ColliderHandle, PhysicsWorld},
};
use crate::{Autostep, InteractionGroups, Length};
use kmath::Vec2;

/// 2D 角色控制器的参数。
///
/// 字段的含义与 [`crate::CharacterController`] 逐条对应，那边的注释说明了
/// 每个参数为什么存在、调错了会怎样。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterController {
    /// 哪个方向算「上」。2D 里通常是 `Vec2::Y`。
    pub up: Vec2,
    /// 角色和周围保持的一点缝隙。**不能为零**。
    pub offset: Length,
    /// 撞到东西时是否沿表面滑过去。
    pub slide: bool,
    /// 自动上台阶。为 [`None`] 时关闭。
    pub autostep: Option<Autostep>,
    /// 能爬多陡的坡（弧度）。
    pub max_slope_climb_angle: f32,
    /// 陡到多少度开始自动往下滑（弧度）。
    pub min_slope_slide_angle: f32,
    /// 离地多近时自动吸附到地面。
    pub snap_to_ground: Option<Length>,
    /// 贴着表面滑动时，沿接触法线额外推开多少。
    ///
    /// **2D 的默认值是 1e-2，比 3D 大一百倍**，这是实测出来的：
    /// 一个胶囊角色站在半宽 50 的地面上往前走，用 3D 的默认值 1e-4
    /// 会在第 8 帧突然完全卡住（水平位移变成 0，而周围什么都没有）；
    /// 把地面缩到半宽 5 就不卡，说明是大尺寸形状下的扫掠精度问题。
    /// 1e-3 仍然卡，1e-2 才彻底解决。
    ///
    /// 代价是角色站得比理论位置高约 1 厘米。
    pub nudge: f32,
    /// 碰撞过滤组。
    pub groups: InteractionGroups,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            up: Vec2::Y,
            offset: Length::Relative(0.01),
            slide: true,
            autostep: Some(Autostep::default()),
            max_slope_climb_angle: std::f32::consts::FRAC_PI_4,
            min_slope_slide_angle: std::f32::consts::FRAC_PI_6,
            snap_to_ground: Some(Length::Relative(0.2)),
            // 见字段注释：3D 的 1e-4 在 2D 里会让角色走几帧就卡死。
            nudge: 1.0e-2,
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

    /// 设置能爬的最大坡度（弧度），并把下滑角度一起夹住。
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

    fn to_rapier(self) -> rapier2d::control::KinematicCharacterController {
        rapier2d::control::KinematicCharacterController {
            up: to_rv(self.up.normalize_or(Vec2::Y)),
            offset: length_to_rapier(self.offset),
            slide: self.slide,
            autostep: self
                .autostep
                .map(|step| rapier2d::control::CharacterAutostep {
                    max_height: length_to_rapier(step.max_height),
                    min_width: length_to_rapier(step.min_width),
                    include_dynamic_bodies: step.include_dynamic_bodies,
                }),
            max_slope_climb_angle: self.max_slope_climb_angle,
            min_slope_slide_angle: self.min_slope_slide_angle.min(self.max_slope_climb_angle),
            snap_to_ground: self.snap_to_ground.map(length_to_rapier),
            normal_nudge_factor: self.nudge,
        }
    }
}

/// [`Length`] 转 rapier2d 的版本。
///
/// 不能和 3D 那份共用：rapier2d 和 rapier3d 的 `CharacterLength`
/// 是两个不同的类型，尽管长得一模一样。
fn length_to_rapier(length: Length) -> rapier2d::control::CharacterLength {
    match length {
        Length::Absolute(value) => rapier2d::control::CharacterLength::Absolute(value),
        Length::Relative(value) => rapier2d::control::CharacterLength::Relative(value),
    }
}

/// 一次 2D 移动的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterMovement {
    /// **实际**发生的位移。
    pub translation: Vec2,
    /// 移动之后是否踩在地上。
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
    pub point: Vec2,
    /// 接触法线（世界空间）。
    pub normal: Vec2,
    /// 撞上时角色已经走了多远。
    pub distance: f32,
}

impl PhysicsWorld {
    /// 移动一个 2D 角色，并把刚体挪过去。
    ///
    /// `body` 应当是运动学刚体且挂着碰撞体。细节与注意事项同
    /// [`crate::PhysicsWorld::move_character`]。
    pub fn move_character(
        &mut self,
        controller: &CharacterController,
        body: BodyHandle,
        desired: Vec2,
        dt: f32,
    ) -> CharacterMovement {
        let movement = self.compute_character_movement(controller, body, desired, dt, &mut |_| {});

        if movement.translation != Vec2::ZERO
            && let Some(mut handle) = self.body_mut(body)
        {
            let position = handle.position() + movement.translation;
            let rotation = handle.rotation();
            handle.set_next_kinematic_position(position, rotation);
        }
        movement
    }

    /// 只算不动，并把沿途撞到的东西报给回调。
    pub fn compute_character_movement(
        &mut self,
        controller: &CharacterController,
        body: BodyHandle,
        desired: Vec2,
        dt: f32,
        on_collision: &mut dyn FnMut(CharacterCollision),
    ) -> CharacterMovement {
        let none = CharacterMovement {
            translation: Vec2::ZERO,
            grounded: false,
            sliding_down_slope: false,
        };

        // 查询走的是广相的 BVH，而 BVH 在 `step` 里维护。刚加完角色就移动
        // 的话，查询树里还没有那些墙——角色会直接穿过去，而且不报错。
        self.update_query_structures();

        let Some(rigid_body) = self.inner.bodies.get(body.0) else {
            return none;
        };
        let Some(collider_handle) = rigid_body.colliders().first().copied() else {
            debug_assert!(false, "角色刚体没有碰撞体，move_character 什么都不会做");
            return none;
        };
        let Some(collider) = self.inner.colliders.get(collider_handle) else {
            return none;
        };

        let shape = collider.shared_shape().clone();
        let pose = *collider.position();
        let native = controller.to_rapier();

        // 排除角色自己：不排除的话第一次扫掠就撞上自己，一步也走不动。
        let filter = rapier2d::prelude::QueryFilter::new()
            .groups(controller.groups.to_rapier2d())
            .exclude_rigid_body(body.0)
            .exclude_collider(collider_handle);

        // 先做一次零位移的扫掠。rapier 只在位移为零时才跑解穿透，
        // 而「把角色正好摆在地面上」是最自然的摆法——那等于一开始就落在
        // 安全边距里面，之后每帧的下坠都不会被挡住，角色会一路陷进地里。
        let queries = self.inner.query_pipeline_with_filter(filter);
        let fix = native.move_shape(dt, &queries, &*shape, &pose, to_rv(Vec2::ZERO), |_| {});
        let pose =
            rapier2d::math::Pose::from_parts(pose.translation + fix.translation, pose.rotation);

        let mut collisions = Vec::new();
        let result = native.move_shape(dt, &queries, &*shape, &pose, to_rv(desired), |collision| {
            collisions.push(collision)
        });

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
                point: from_rv(collision.hit.witness1),
                normal: from_rv(collision.hit.normal1),
                distance: collision.hit.time_of_impact,
            });
        }

        CharacterMovement {
            translation: from_rv(fix.translation) + from_rv(result.translation),
            grounded: result.grounded,
            sliding_down_slope: result.is_sliding_down_slope,
        }
    }
}
