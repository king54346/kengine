//! 刚体：描述、类型、以及访问原生刚体的借用包装。

use crate::convert::{from_rq, from_rv, to_rp, to_rv};
use kmath::{Quat, Vec3};
use rapier3d::dynamics as rd;

/// 刚体在模拟中的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RigidBodyType {
    /// 受力与碰撞驱动，会被重力拉下去。绝大多数会动的东西都是这个。
    #[default]
    Dynamic,
    /// 完全不动。地面、墙壁。质量视作无穷大。
    Fixed,
    /// 由外部**位置**驱动。你每帧设它的位置，它推开动态物体而不被推动。
    /// 平台、电梯、角色控制器用这个。
    KinematicPositionBased,
    /// 由外部**速度**驱动。你设速度，引擎按速度积分出位置。
    KinematicVelocityBased,
}

impl RigidBodyType {
    /// 是否是两种运动学刚体之一。
    pub fn is_kinematic(self) -> bool {
        matches!(
            self,
            RigidBodyType::KinematicPositionBased | RigidBodyType::KinematicVelocityBased
        )
    }

    pub(crate) fn to_rapier(self) -> rd::RigidBodyType {
        match self {
            RigidBodyType::Dynamic => rd::RigidBodyType::Dynamic,
            RigidBodyType::Fixed => rd::RigidBodyType::Fixed,
            RigidBodyType::KinematicPositionBased => rd::RigidBodyType::KinematicPositionBased,
            RigidBodyType::KinematicVelocityBased => rd::RigidBodyType::KinematicVelocityBased,
        }
    }

    /// 同上，2D 版。rapier2d 和 rapier3d 的这个枚举是两个不同的类型。
    pub(crate) fn to_rapier2d(self) -> rapier2d::dynamics::RigidBodyType {
        use rapier2d::dynamics::RigidBodyType as R;
        match self {
            RigidBodyType::Dynamic => R::Dynamic,
            RigidBodyType::Fixed => R::Fixed,
            RigidBodyType::KinematicPositionBased => R::KinematicPositionBased,
            RigidBodyType::KinematicVelocityBased => R::KinematicVelocityBased,
        }
    }

    /// 同上，2D 版。
    pub(crate) fn from_rapier2d(t: rapier2d::dynamics::RigidBodyType) -> Self {
        use rapier2d::dynamics::RigidBodyType as R;
        match t {
            R::Dynamic => RigidBodyType::Dynamic,
            R::Fixed => RigidBodyType::Fixed,
            R::KinematicPositionBased => RigidBodyType::KinematicPositionBased,
            R::KinematicVelocityBased => RigidBodyType::KinematicVelocityBased,
        }
    }

    pub(crate) fn from_rapier(t: rd::RigidBodyType) -> Self {
        match t {
            rd::RigidBodyType::Dynamic => RigidBodyType::Dynamic,
            rd::RigidBodyType::Fixed => RigidBodyType::Fixed,
            rd::RigidBodyType::KinematicPositionBased => RigidBodyType::KinematicPositionBased,
            rd::RigidBodyType::KinematicVelocityBased => RigidBodyType::KinematicVelocityBased,
        }
    }
}

/// 建一个刚体所需的全部参数。
///
/// 这是一份**纯数据**的描述，可以在没有物理世界的情况下构造、比较、序列化；
/// 真正的原生刚体由 [`PhysicsWorld::add_body`](crate::PhysicsWorld::add_body) 按它造出来。
#[derive(Debug, Clone, PartialEq)]
pub struct RigidBodyDesc {
    /// 刚体类型。
    pub body_type: RigidBodyType,
    /// 世界空间位置。
    pub position: Vec3,
    /// 世界空间朝向。
    pub rotation: Quat,
    /// 初始线速度。
    pub linvel: Vec3,
    /// 初始角速度（轴 × 弧度/秒）。
    pub angvel: Vec3,
    /// 线性阻尼，模拟空气阻力。0 表示不衰减。
    pub linear_damping: f32,
    /// 角阻尼。
    pub angular_damping: f32,
    /// 重力倍率。0 = 失重，负数 = 上浮。
    pub gravity_scale: f32,
    /// 在碰撞体算出的质量之上**额外**叠加的质量。
    ///
    /// 不是「总质量」——碰撞体按密度贡献的那份仍然算数。
    /// 想完全指定质量应该改碰撞体的密度。
    pub additional_mass: f32,
    /// 锁住的平移轴（X/Y/Z）。锁住的轴上物体不会移动。
    pub locked_translations: [bool; 3],
    /// 锁住的旋转轴（X/Y/Z）。全锁上就是「不会倒的胶囊」，角色常用。
    pub locked_rotations: [bool; 3],
    /// 是否开启连续碰撞检测。高速小物体穿墙时才需要，代价不小。
    pub ccd_enabled: bool,
    /// 是否允许长时间静止后休眠。休眠的刚体不参与求解，是大场景的主要省电手段。
    pub can_sleep: bool,
    /// 支配组。高支配组的刚体撞低支配组时**不会**被反推，用来做「推不动的主角」。
    pub dominance_group: i8,
    /// 是否启用。禁用的刚体连同其碰撞体一起退出模拟，但句柄仍然有效。
    pub enabled: bool,
}

impl Default for RigidBodyDesc {
    fn default() -> Self {
        Self {
            body_type: RigidBodyType::Dynamic,
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            linvel: Vec3::ZERO,
            angvel: Vec3::ZERO,
            linear_damping: 0.0,
            angular_damping: 0.0,
            gravity_scale: 1.0,
            additional_mass: 0.0,
            locked_translations: [false; 3],
            locked_rotations: [false; 3],
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
            ..Self::default()
        }
    }

    /// 一个按位置驱动的运动学刚体。
    pub fn kinematic_position_based() -> Self {
        Self {
            body_type: RigidBodyType::KinematicPositionBased,
            ..Self::default()
        }
    }

    /// 一个按速度驱动的运动学刚体。
    pub fn kinematic_velocity_based() -> Self {
        Self {
            body_type: RigidBodyType::KinematicVelocityBased,
            ..Self::default()
        }
    }

    /// 指定初始位置。
    pub fn with_position(mut self, position: Vec3) -> Self {
        self.position = position;
        self
    }

    /// 指定初始朝向。
    pub fn with_rotation(mut self, rotation: Quat) -> Self {
        self.rotation = rotation;
        self
    }

    /// 指定初始线速度。
    pub fn with_linvel(mut self, linvel: Vec3) -> Self {
        self.linvel = linvel;
        self
    }

    /// 指定初始角速度。
    pub fn with_angvel(mut self, angvel: Vec3) -> Self {
        self.angvel = angvel;
        self
    }

    /// 指定线性与角阻尼。
    pub fn with_damping(mut self, linear: f32, angular: f32) -> Self {
        self.linear_damping = linear;
        self.angular_damping = angular;
        self
    }

    /// 指定重力倍率。
    pub fn with_gravity_scale(mut self, scale: f32) -> Self {
        self.gravity_scale = scale;
        self
    }

    /// 追加质量。
    pub fn with_additional_mass(mut self, mass: f32) -> Self {
        self.additional_mass = mass;
        self
    }

    /// 锁住全部三个旋转轴。角色胶囊几乎总要这个，否则一撞就躺倒。
    pub fn with_locked_rotations(mut self) -> Self {
        self.locked_rotations = [true; 3];
        self
    }

    /// 开启连续碰撞检测。
    pub fn with_ccd(mut self, enabled: bool) -> Self {
        self.ccd_enabled = enabled;
        self
    }

    /// 设置是否允许休眠。
    pub fn with_can_sleep(mut self, can_sleep: bool) -> Self {
        self.can_sleep = can_sleep;
        self
    }

    pub(crate) fn build(&self, user_data: u128) -> rd::RigidBody {
        let mut locked = rd::LockedAxes::empty();
        for (axis, flag) in [
            rd::LockedAxes::TRANSLATION_LOCKED_X,
            rd::LockedAxes::TRANSLATION_LOCKED_Y,
            rd::LockedAxes::TRANSLATION_LOCKED_Z,
        ]
        .into_iter()
        .zip(self.locked_translations)
        {
            if flag {
                locked |= axis;
            }
        }
        for (axis, flag) in [
            rd::LockedAxes::ROTATION_LOCKED_X,
            rd::LockedAxes::ROTATION_LOCKED_Y,
            rd::LockedAxes::ROTATION_LOCKED_Z,
        ]
        .into_iter()
        .zip(self.locked_rotations)
        {
            if flag {
                locked |= axis;
            }
        }

        let mut body = rd::RigidBodyBuilder::new(self.body_type.to_rapier())
            .pose(to_rp(self.position, self.rotation))
            .linvel(to_rv(self.linvel))
            .angvel(to_rv(self.angvel))
            .linear_damping(self.linear_damping)
            .angular_damping(self.angular_damping)
            .gravity_scale(self.gravity_scale)
            .additional_mass(self.additional_mass)
            .locked_axes(locked)
            .ccd_enabled(self.ccd_enabled)
            .can_sleep(self.can_sleep)
            .dominance_group(self.dominance_group)
            .enabled(self.enabled)
            .user_data(user_data)
            .build();

        // `can_sleep(false)` 只是把休眠阈值设成负数；已经在睡的刚体要显式叫醒。
        if !self.can_sleep {
            body.wake_up(true);
        }
        body
    }
}

/// 原生刚体的只读视图。所有返回值都已换成 kmath 类型。
pub struct BodyRef<'a>(pub(crate) &'a rd::RigidBody);

impl BodyRef<'_> {
    /// 位置与朝向一次取出。
    ///
    /// 同步回场景图时两者总是一起要的，分两次取会白查两遍。
    pub fn pose(&self) -> (Vec3, Quat) {
        crate::convert::from_rp(self.0.position())
    }

    /// 世界空间位置。
    pub fn position(&self) -> Vec3 {
        from_rv(self.0.position().translation)
    }

    /// 世界空间朝向。
    pub fn rotation(&self) -> Quat {
        from_rq(*self.0.rotation())
    }

    /// 线速度。
    pub fn linvel(&self) -> Vec3 {
        from_rv(self.0.linvel())
    }

    /// 角速度。
    pub fn angvel(&self) -> Vec3 {
        from_rv(self.0.angvel())
    }

    /// 刚体类型。
    pub fn body_type(&self) -> RigidBodyType {
        RigidBodyType::from_rapier(self.0.body_type())
    }

    /// 总质量（碰撞体贡献 + 追加质量）。静态刚体为 0。
    pub fn mass(&self) -> f32 {
        self.0.mass()
    }

    /// 是否正在休眠。
    pub fn is_sleeping(&self) -> bool {
        self.0.is_sleeping()
    }

    /// 是否启用。
    pub fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    /// 创建时写入的用户数据。
    pub fn user_data(&self) -> u128 {
        self.0.user_data
    }
}

/// 原生刚体的可变视图。
pub struct BodyMut<'a>(pub(crate) &'a mut rd::RigidBody);

impl BodyMut<'_> {
    /// 降级成只读视图，复用它的读取方法。
    pub fn as_ref(&self) -> BodyRef<'_> {
        BodyRef(self.0)
    }

    /// 世界空间位置。
    pub fn position(&self) -> Vec3 {
        self.as_ref().position()
    }

    /// 世界空间朝向。
    pub fn rotation(&self) -> Quat {
        self.as_ref().rotation()
    }

    /// 位置与朝向一次取出。
    pub fn pose(&self) -> (Vec3, Quat) {
        self.as_ref().pose()
    }

    /// 线速度。
    pub fn linvel(&self) -> Vec3 {
        self.as_ref().linvel()
    }

    /// 角速度。
    pub fn angvel(&self) -> Vec3 {
        self.as_ref().angvel()
    }

    /// 刚体类型。
    pub fn body_type(&self) -> RigidBodyType {
        self.as_ref().body_type()
    }

    /// 是否正在休眠。
    pub fn is_sleeping(&self) -> bool {
        self.as_ref().is_sleeping()
    }

    /// **瞬移**到指定位姿。
    ///
    /// 这是硬设置，不产生速度，也不做扫掠检测——中间挡着什么都会被穿过去。
    /// 运动学刚体想要「推开挡路的东西」应该用
    /// [`set_next_kinematic_position`](Self::set_next_kinematic_position)。
    pub fn set_position(&mut self, position: Vec3, rotation: Quat, wake_up: bool) {
        self.0.set_position(to_rp(position, rotation), wake_up);
    }

    /// 设定运动学刚体**下一步**的目标位姿。
    ///
    /// 引擎会由「当前 → 目标」反推出速度，于是路上的动态物体会被正确推开。
    /// 对非运动学刚体无效。
    pub fn set_next_kinematic_position(&mut self, position: Vec3, rotation: Quat) {
        self.0
            .set_next_kinematic_position(to_rp(position, rotation));
    }

    /// 设置线速度。
    pub fn set_linvel(&mut self, linvel: Vec3, wake_up: bool) {
        self.0.set_linvel(to_rv(linvel), wake_up);
    }

    /// 设置角速度。
    pub fn set_angvel(&mut self, angvel: Vec3, wake_up: bool) {
        self.0.set_angvel(to_rv(angvel), wake_up);
    }

    /// 切换刚体类型。
    pub fn set_body_type(&mut self, body_type: RigidBodyType, wake_up: bool) {
        self.0.set_body_type(body_type.to_rapier(), wake_up);
    }

    /// 启用 / 禁用。禁用后该刚体与其碰撞体一起退出模拟。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.0.set_enabled(enabled);
    }

    /// 设置重力倍率。
    pub fn set_gravity_scale(&mut self, scale: f32, wake_up: bool) {
        self.0.set_gravity_scale(scale, wake_up);
    }

    /// 施加持续力，只在**当前这一步**有效，步进后自动清零。
    pub fn add_force(&mut self, force: Vec3, wake_up: bool) {
        self.0.add_force(to_rv(force), wake_up);
    }

    /// 施加持续力矩，同样只作用一步。
    pub fn add_torque(&mut self, torque: Vec3, wake_up: bool) {
        self.0.add_torque(to_rv(torque), wake_up);
    }

    /// 在世界空间某点施加持续力，会同时产生力矩。
    pub fn add_force_at_point(&mut self, force: Vec3, point: Vec3, wake_up: bool) {
        self.0
            .add_force_at_point(to_rv(force), to_rv(point), wake_up);
    }

    /// 施加瞬时冲量，立即改变速度（Δv = 冲量 / 质量）。爆炸、跳跃、击飞用这个。
    pub fn apply_impulse(&mut self, impulse: Vec3, wake_up: bool) {
        self.0.apply_impulse(to_rv(impulse), wake_up);
    }

    /// 施加瞬时角冲量。
    pub fn apply_torque_impulse(&mut self, impulse: Vec3, wake_up: bool) {
        self.0.apply_torque_impulse(to_rv(impulse), wake_up);
    }

    /// 在世界空间某点施加瞬时冲量。偏离质心时会让物体转起来。
    pub fn apply_impulse_at_point(&mut self, impulse: Vec3, point: Vec3, wake_up: bool) {
        self.0
            .apply_impulse_at_point(to_rv(impulse), to_rv(point), wake_up);
    }

    /// 唤醒。`strong` 为真时会把休眠计时器完全清零。
    pub fn wake_up(&mut self, strong: bool) {
        self.0.wake_up(strong);
    }

    /// 强制休眠。
    pub fn sleep(&mut self) {
        self.0.sleep();
    }

    /// 设置追加质量。
    pub fn set_additional_mass(&mut self, mass: f32, wake_up: bool) {
        self.0.set_additional_mass(mass, wake_up);
    }

    /// 设置线性阻尼。
    pub fn set_linear_damping(&mut self, damping: f32) {
        self.0.set_linear_damping(damping);
    }

    /// 设置角阻尼。
    pub fn set_angular_damping(&mut self, damping: f32) {
        self.0.set_angular_damping(damping);
    }

    /// 开关连续碰撞检测。
    pub fn enable_ccd(&mut self, enabled: bool) {
        self.0.enable_ccd(enabled);
    }

    /// 锁住 / 放开三个旋转轴。
    pub fn set_locked_rotations(&mut self, locked: [bool; 3], wake_up: bool) {
        self.0
            .set_enabled_rotations(!locked[0], !locked[1], !locked[2], wake_up);
    }

    /// 锁住 / 放开三个平移轴。
    pub fn set_locked_translations(&mut self, locked: [bool; 3], wake_up: bool) {
        self.0
            .set_enabled_translations(!locked[0], !locked[1], !locked[2], wake_up);
    }

    /// 世界空间中某点随刚体运动的速度（含自转贡献）。
    pub fn velocity_at_point(&self, point: Vec3) -> Vec3 {
        from_rv(self.0.velocity_at_point(to_rv(point)))
    }

    /// 用户数据。
    pub fn user_data(&self) -> u128 {
        self.0.user_data
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn default_desc_is_a_plain_dynamic_body() {
        let desc = RigidBodyDesc::default();

        assert_eq!(desc.body_type, RigidBodyType::Dynamic);
        assert_eq!(desc.gravity_scale, 1.0);
        assert!(desc.can_sleep);
    }

    #[test]
    fn body_type_roundtrips_through_rapier() {
        // 四种类型一一对应，漏一个就会把运动学当成静态。
        for t in [
            RigidBodyType::Dynamic,
            RigidBodyType::Fixed,
            RigidBodyType::KinematicPositionBased,
            RigidBodyType::KinematicVelocityBased,
        ] {
            assert_eq!(RigidBodyType::from_rapier(t.to_rapier()), t);
        }
    }

    #[test]
    fn only_kinematic_types_report_kinematic() {
        assert!(RigidBodyType::KinematicPositionBased.is_kinematic());
        assert!(RigidBodyType::KinematicVelocityBased.is_kinematic());
        assert!(!RigidBodyType::Dynamic.is_kinematic());
        assert!(!RigidBodyType::Fixed.is_kinematic());
    }

    #[test]
    fn locked_axes_map_to_the_matching_rapier_flags() {
        // 平移和旋转的标志位很容易串——分开锁，分别验。
        let desc = RigidBodyDesc::dynamic();
        let mut only_y = desc.clone();
        only_y.locked_translations = [false, true, false];
        let body = only_y.build(0);
        assert!(
            body.locked_axes()
                .contains(rd::LockedAxes::TRANSLATION_LOCKED_Y)
        );
        assert!(
            !body
                .locked_axes()
                .contains(rd::LockedAxes::TRANSLATION_LOCKED_X)
        );
        assert!(
            !body
                .locked_axes()
                .contains(rd::LockedAxes::ROTATION_LOCKED_Y)
        );

        let all_rotations = desc.with_locked_rotations().build(0);
        assert!(all_rotations.is_rotation_locked().iter().all(|l| *l));
    }

    #[test]
    fn build_carries_position_and_velocity_over() {
        let desc = RigidBodyDesc::dynamic()
            .with_position(Vec3::new(1.0, 2.0, 3.0))
            .with_linvel(Vec3::new(0.0, -4.0, 0.0));
        let body = desc.build(0x1234);

        assert_eq!(BodyRef(&body).position(), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(BodyRef(&body).linvel(), Vec3::new(0.0, -4.0, 0.0));
        assert_eq!(BodyRef(&body).user_data(), 0x1234);
    }

    #[test]
    fn no_sleep_bodies_start_awake() {
        // can_sleep(false) 在 rapier 里只是把阈值设负；不显式唤醒的话，
        // 建出来的刚体可能一开始就是睡着的，加了力也不动。
        let body = RigidBodyDesc::dynamic().with_can_sleep(false).build(0);
        assert!(!body.is_sleeping());
    }
}
