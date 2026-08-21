//! 2D 刚体：描述与借用包装。

use super::convert::{from_rp, from_rv, to_rp, to_rv};
use crate::RigidBodyType;
use kmath::Vec2;
use rapier2d::dynamics as rd;

/// 建一个 2D 刚体所需的全部参数。
///
/// 和 3D 的 [`RigidBodyDesc`](crate::RigidBodyDesc) 一一对应，只是
/// 位置是 [`Vec2`]、角速度是标量、锁轴只有两个平移方向和一个旋转开关。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidBodyDesc {
    /// 刚体类型。
    pub body_type: RigidBodyType,
    /// 世界空间位置。
    pub position: Vec2,
    /// 世界空间朝向，弧度。
    pub rotation: f32,
    /// 初始线速度。
    pub linvel: Vec2,
    /// 初始角速度，弧度/秒。
    ///
    /// 2D 里只有一个旋转自由度（绕垂直于平面的轴），所以是标量。
    pub angvel: f32,
    /// 线性阻尼，模拟空气阻力。0 表示不衰减。
    pub linear_damping: f32,
    /// 角阻尼。
    pub angular_damping: f32,
    /// 重力倍率。0 = 失重，负数 = 上浮。
    pub gravity_scale: f32,
    /// 在碰撞体算出的质量之上**额外**叠加的质量。
    pub additional_mass: f32,
    /// 锁住的平移轴（X/Y）。
    pub locked_translations: [bool; 2],
    /// 锁住旋转。开着就是「不会倒的方块」——平台跳跃游戏的主角基本都要这个，
    /// 否则一撞墙就开始打转。
    pub locked_rotation: bool,
    /// 是否开启连续碰撞检测。高速小物体穿墙时才需要，代价不小。
    pub ccd_enabled: bool,
    /// 是否允许长时间静止后休眠。
    pub can_sleep: bool,
    /// 支配组。高支配组的刚体撞低支配组时**不会**被反推。
    pub dominance_group: i8,
    /// 是否启用。
    pub enabled: bool,
}

impl Default for RigidBodyDesc {
    fn default() -> Self {
        Self {
            body_type: RigidBodyType::Dynamic,
            position: Vec2::ZERO,
            rotation: 0.0,
            linvel: Vec2::ZERO,
            angvel: 0.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            gravity_scale: 1.0,
            additional_mass: 0.0,
            locked_translations: [false; 2],
            locked_rotation: false,
            ccd_enabled: false,
            can_sleep: true,
            dominance_group: 0,
            enabled: true,
        }
    }
}

impl RigidBodyDesc {
    /// 一个动态刚体。
    pub fn dynamic() -> Self {
        Self::default()
    }

    /// 一个静态刚体。
    pub fn fixed() -> Self {
        Self {
            body_type: RigidBodyType::Fixed,
            ..Default::default()
        }
    }

    /// 一个由位置驱动的运动学刚体。
    pub fn kinematic_position() -> Self {
        Self {
            body_type: RigidBodyType::KinematicPositionBased,
            ..Default::default()
        }
    }

    /// 一个由速度驱动的运动学刚体。
    pub fn kinematic_velocity() -> Self {
        Self {
            body_type: RigidBodyType::KinematicVelocityBased,
            ..Default::default()
        }
    }

    /// 设置位置。
    pub fn with_position(mut self, position: Vec2) -> Self {
        self.position = position;
        self
    }

    /// 设置朝向（弧度）。
    pub fn with_rotation(mut self, rotation: f32) -> Self {
        self.rotation = rotation;
        self
    }

    /// 设置初始线速度。
    pub fn with_linvel(mut self, linvel: Vec2) -> Self {
        self.linvel = linvel;
        self
    }

    /// 锁住旋转。平台跳跃的主角基本都要这个。
    pub fn with_locked_rotation(mut self) -> Self {
        self.locked_rotation = true;
        self
    }

    /// 设置重力倍率。
    pub fn with_gravity_scale(mut self, scale: f32) -> Self {
        self.gravity_scale = scale;
        self
    }

    pub(crate) fn build(&self, user_data: u128) -> rd::RigidBody {
        let mut builder = rd::RigidBodyBuilder::new(self.body_type.to_rapier2d())
            .pose(to_rp(self.position, self.rotation))
            .linvel(to_rv(self.linvel))
            .angvel(self.angvel)
            .linear_damping(self.linear_damping)
            .angular_damping(self.angular_damping)
            .gravity_scale(self.gravity_scale)
            .additional_mass(self.additional_mass)
            .ccd_enabled(self.ccd_enabled)
            .can_sleep(self.can_sleep)
            .dominance_group(self.dominance_group)
            .enabled(self.enabled)
            .user_data(user_data);

        if self.locked_rotation {
            builder = builder.lock_rotations();
        }
        builder = builder
            .enabled_translations(!self.locked_translations[0], !self.locked_translations[1]);
        builder.build()
    }
}

/// 只读地看一个 2D 刚体。
pub struct BodyRef<'a> {
    pub(crate) inner: &'a rd::RigidBody,
}

impl BodyRef<'_> {
    /// 世界空间位置。
    pub fn position(&self) -> Vec2 {
        from_rv(self.inner.position().translation)
    }

    /// 世界空间朝向，弧度。
    pub fn rotation(&self) -> f32 {
        from_rp(self.inner.position()).1
    }

    /// 线速度。
    pub fn linvel(&self) -> Vec2 {
        from_rv(self.inner.linvel())
    }

    /// 角速度，弧度/秒。
    pub fn angvel(&self) -> f32 {
        self.inner.angvel()
    }

    /// 质量。静态刚体是 0。
    pub fn mass(&self) -> f32 {
        self.inner.mass()
    }

    /// 刚体类型。
    pub fn body_type(&self) -> RigidBodyType {
        RigidBodyType::from_rapier2d(self.inner.body_type())
    }

    /// 是否在休眠。
    pub fn is_sleeping(&self) -> bool {
        self.inner.is_sleeping()
    }

    /// 是否启用。
    pub fn is_enabled(&self) -> bool {
        self.inner.is_enabled()
    }

    /// 建它时塞进去的用户数据。`kscene` 往里放节点句柄。
    pub fn user_data(&self) -> u128 {
        self.inner.user_data
    }
}

/// 可写地操作一个 2D 刚体。
pub struct BodyMut<'a> {
    pub(crate) inner: &'a mut rd::RigidBody,
}

impl BodyMut<'_> {
    /// 世界空间位置。
    pub fn position(&self) -> Vec2 {
        from_rv(self.inner.position().translation)
    }

    /// 世界空间朝向，弧度。
    pub fn rotation(&self) -> f32 {
        from_rp(self.inner.position()).1
    }

    /// 线速度。
    pub fn linvel(&self) -> Vec2 {
        from_rv(self.inner.linvel())
    }

    /// 角速度。
    pub fn angvel(&self) -> f32 {
        self.inner.angvel()
    }

    /// 直接设置位置与朝向。
    ///
    /// `wake` 为真时顺便唤醒刚体。**动态刚体一般要唤醒**——传送一个正在
    /// 休眠的物体，它会停在新位置一动不动，直到有别的东西撞它。
    pub fn set_position(&mut self, position: Vec2, rotation: f32, wake: bool) {
        self.inner.set_position(to_rp(position, rotation), wake);
    }

    /// 设置运动学刚体的目标位姿。
    ///
    /// 和 [`set_position`](Self::set_position) 的区别：这个让引擎在下一步里
    /// **插值过去**，于是刚体沿途会正确地推开动态物体。直接设位置的话
    /// 它会瞬移，路上的东西可能被漏掉或被挤穿。
    pub fn set_next_kinematic_position(&mut self, position: Vec2, rotation: f32) {
        self.inner
            .set_next_kinematic_position(to_rp(position, rotation));
    }

    /// 设置线速度。
    pub fn set_linvel(&mut self, linvel: Vec2, wake: bool) {
        self.inner.set_linvel(to_rv(linvel), wake);
    }

    /// 设置角速度。
    pub fn set_angvel(&mut self, angvel: f32, wake: bool) {
        self.inner.set_angvel(angvel, wake);
    }

    /// 施加一个力，下一步生效。
    pub fn add_force(&mut self, force: Vec2, wake: bool) {
        self.inner.add_force(to_rv(force), wake);
    }

    /// 施加一个冲量，立刻改变速度。
    ///
    /// 力和冲量的区别：力要乘以时间步才变成速度，冲量直接就是动量。
    /// 跳跃用冲量（一瞬间的事），风用力（持续的）。
    pub fn apply_impulse(&mut self, impulse: Vec2, wake: bool) {
        self.inner.apply_impulse(to_rv(impulse), wake);
    }

    /// 在某一点施加冲量，会同时产生旋转。
    pub fn apply_impulse_at_point(&mut self, impulse: Vec2, point: Vec2, wake: bool) {
        self.inner
            .apply_impulse_at_point(to_rv(impulse), to_rv(point), wake);
    }

    /// 施加一个扭矩冲量，直接改变角速度。
    pub fn apply_torque_impulse(&mut self, torque: f32, wake: bool) {
        self.inner.apply_torque_impulse(torque, wake);
    }

    /// 清掉累积的力与扭矩。
    pub fn reset_forces(&mut self, wake: bool) {
        self.inner.reset_forces(wake);
        self.inner.reset_torques(wake);
    }

    /// 改刚体类型。
    pub fn set_body_type(&mut self, body_type: RigidBodyType, wake: bool) {
        self.inner.set_body_type(body_type.to_rapier2d(), wake);
    }

    /// 唤醒。
    pub fn wake_up(&mut self, strong: bool) {
        self.inner.wake_up(strong);
    }

    /// 启用 / 禁用。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.inner.set_enabled(enabled);
    }

    /// 设置重力倍率。
    pub fn set_gravity_scale(&mut self, scale: f32, wake: bool) {
        self.inner.set_gravity_scale(scale, wake);
    }

    /// 锁 / 解锁旋转。
    pub fn set_locked_rotation(&mut self, locked: bool, wake: bool) {
        self.inner.lock_rotations(locked, wake);
    }
}
