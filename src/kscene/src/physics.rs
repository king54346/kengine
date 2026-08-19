//! 物理组件：把 `kphysics` 的刚体 / 碰撞体 / 关节挂到场景节点上。
//!
//! # 谁驱动谁
//!
//! 这是整套同步逻辑里唯一需要记住的事，方向搞反了症状会非常费解：
//!
//! | 组件 | 方向 |
//! |---|---|
//! | 动态刚体 | **物理 → 场景图**。每帧把模拟结果写回节点的局部变换 |
//! | 静态 / 运动学刚体 | **场景图 → 物理**。改节点变换就是在驱动它们 |
//! | 碰撞体（无刚体） | **场景图 → 物理** |
//!
//! 因此动态刚体的位置**不能**靠改 `node.transform` 来设——下一次同步就被
//! 模拟结果覆盖回去了。要瞬移得用 [`RigidBody::teleport`]，
//! 要推它得用 [`RigidBody::apply_impulse`] 之类。Fyrox 的语义与此一致。
//!
//! # 碰撞体挂在哪
//!
//! 碰撞体绑定到**自己或最近的祖先**中带刚体的那个节点；一个都没有就是
//! 一块不动的静态几何。于是两种写法都成立：简单物体把刚体和碰撞体挂同一个
//! 节点上；复合物体给刚体节点挂若干个带碰撞体的子节点，各自带偏移。

use kcore::pool::Handle;
use kmath::{Quat, Vec3};
use kphysics::{
    BodyHandle, ColliderDesc, ColliderHandle, ColliderShape, JointDesc, JointHandle, PhysicsWorld,
    RigidBodyDesc, RigidBodyType,
};

use crate::Node;

/// 排队等下一次同步时执行的刚体操作。
///
/// 为什么要排队：用户代码在 `update` 里调 `apply_impulse` 时，原生刚体可能
/// 还没建出来（节点是这一帧刚加的），也可能正被物理世界借着。排进队列里，
/// 到同步那一刻统一执行，调用方就不必关心时序。Fyrox 的 `RigidBody::actions`
/// 是同一个思路。
#[derive(Debug, Clone, Copy, PartialEq)]
enum BodyAction {
    ApplyImpulse(Vec3),
    ApplyTorqueImpulse(Vec3),
    ApplyImpulseAtPoint(Vec3, Vec3),
    AddForce(Vec3),
    AddTorque(Vec3),
    SetLinvel(Vec3),
    SetAngvel(Vec3),
    Teleport(Vec3, Quat),
    WakeUp,
    Sleep,
}

/// 挂在节点上的刚体。
#[derive(Debug, Clone)]
pub struct RigidBody {
    desc: RigidBodyDesc,
    native: Option<BodyHandle>,
    actions: Vec<BodyAction>,
    /// `desc` 被改过，下次同步要整体重推给原生刚体。
    ///
    /// 不做逐字段的变更追踪：物理节点通常只有几十个，整体重推的代价远小于
    /// 为每个字段维护一个脏标记带来的复杂度。
    desc_dirty: bool,
    /// 以下三项是从原生刚体回读的，只读。
    linvel: Vec3,
    angvel: Vec3,
    sleeping: bool,
}

impl RigidBody {
    /// 按描述创建。位置与朝向会被节点的世界变换覆盖，可以留空。
    pub fn new(desc: RigidBodyDesc) -> Self {
        Self {
            desc,
            native: None,
            actions: Vec::new(),
            desc_dirty: false,
            linvel: Vec3::ZERO,
            angvel: Vec3::ZERO,
            sleeping: false,
        }
    }

    /// 一个动态刚体。
    pub fn dynamic() -> Self {
        Self::new(RigidBodyDesc::dynamic())
    }

    /// 一个静态刚体。
    pub fn fixed() -> Self {
        Self::new(RigidBodyDesc::fixed())
    }

    /// 一个按位置驱动的运动学刚体。位置由节点的变换说了算。
    pub fn kinematic() -> Self {
        Self::new(RigidBodyDesc::kinematic_position_based())
    }

    /// 建体参数的只读引用。
    pub fn desc(&self) -> &RigidBodyDesc {
        &self.desc
    }

    /// 建体参数的可变引用。改完会在下次同步时整体重推给原生刚体。
    ///
    /// 位置与朝向除外——它们由 [`teleport`](Self::teleport) 或场景图负责，
    /// 改 `desc.position` 不会有任何效果。
    pub fn desc_mut(&mut self) -> &mut RigidBodyDesc {
        self.desc_dirty = true;
        &mut self.desc
    }

    /// 刚体类型。
    pub fn body_type(&self) -> RigidBodyType {
        self.desc.body_type
    }

    /// 切换刚体类型。布娃娃的开关就是靠它在动态与运动学之间切。
    pub fn set_body_type(&mut self, body_type: RigidBodyType) {
        if self.desc.body_type != body_type {
            self.desc.body_type = body_type;
            self.desc_dirty = true;
        }
    }

    /// 上一次同步时读到的线速度。
    pub fn linvel(&self) -> Vec3 {
        self.linvel
    }

    /// 上一次同步时读到的角速度。
    pub fn angvel(&self) -> Vec3 {
        self.angvel
    }

    /// 上一次同步时刚体是否在休眠。
    pub fn is_sleeping(&self) -> bool {
        self.sleeping
    }

    /// 对应的原生刚体句柄；还没建出来时为 `None`。
    pub fn native(&self) -> Option<BodyHandle> {
        self.native
    }

    /// 设置线速度。
    pub fn set_linvel(&mut self, linvel: Vec3) {
        self.actions.push(BodyAction::SetLinvel(linvel));
    }

    /// 设置角速度。
    pub fn set_angvel(&mut self, angvel: Vec3) {
        self.actions.push(BodyAction::SetAngvel(angvel));
    }

    /// 施加瞬时冲量，立即改变速度。
    pub fn apply_impulse(&mut self, impulse: Vec3) {
        self.actions.push(BodyAction::ApplyImpulse(impulse));
    }

    /// 施加瞬时角冲量。
    pub fn apply_torque_impulse(&mut self, impulse: Vec3) {
        self.actions.push(BodyAction::ApplyTorqueImpulse(impulse));
    }

    /// 在世界空间某点施加瞬时冲量，会让物体转起来。
    pub fn apply_impulse_at_point(&mut self, impulse: Vec3, point: Vec3) {
        self.actions
            .push(BodyAction::ApplyImpulseAtPoint(impulse, point));
    }

    /// 施加持续力，只作用于下一步。要持续推就每帧调。
    pub fn add_force(&mut self, force: Vec3) {
        self.actions.push(BodyAction::AddForce(force));
    }

    /// 施加持续力矩，只作用于下一步。
    pub fn add_torque(&mut self, torque: Vec3) {
        self.actions.push(BodyAction::AddTorque(torque));
    }

    /// 瞬移到世界空间的指定位姿。
    ///
    /// 动态刚体想改位置**只能**走这里：直接改 `node.transform` 会在下一次
    /// 同步时被模拟结果覆盖掉。
    pub fn teleport(&mut self, position: Vec3, rotation: Quat) {
        self.actions.push(BodyAction::Teleport(position, rotation));
    }

    /// 唤醒。休眠的刚体不参与求解，改了它的属性后往往要叫醒。
    pub fn wake_up(&mut self) {
        self.actions.push(BodyAction::WakeUp);
    }

    /// 强制休眠。
    pub fn sleep(&mut self) {
        self.actions.push(BodyAction::Sleep);
    }

    /// 把排队的操作与改过的属性推给原生刚体。
    pub(crate) fn flush(&mut self, world: &mut PhysicsWorld) {
        let Some(handle) = self.native else {
            // 原生刚体还没建出来。动作留在队列里等下一次，别丢。
            return;
        };

        if self.desc_dirty {
            self.desc_dirty = false;
            if let Some(mut body) = world.body_mut(handle) {
                body.set_body_type(self.desc.body_type, true);
                body.set_gravity_scale(self.desc.gravity_scale, true);
                body.set_linear_damping(self.desc.linear_damping);
                body.set_angular_damping(self.desc.angular_damping);
                body.set_additional_mass(self.desc.additional_mass, true);
                body.enable_ccd(self.desc.ccd_enabled);
                body.set_locked_rotations(self.desc.locked_rotations, true);
                body.set_locked_translations(self.desc.locked_translations, true);
                body.set_enabled(self.desc.enabled);
            }
        }

        if self.actions.is_empty() {
            return;
        }
        let Some(mut body) = world.body_mut(handle) else {
            self.actions.clear();
            return;
        };
        for action in self.actions.drain(..) {
            match action {
                BodyAction::ApplyImpulse(v) => body.apply_impulse(v, true),
                BodyAction::ApplyTorqueImpulse(v) => body.apply_torque_impulse(v, true),
                BodyAction::ApplyImpulseAtPoint(v, p) => body.apply_impulse_at_point(v, p, true),
                BodyAction::AddForce(v) => body.add_force(v, true),
                BodyAction::AddTorque(v) => body.add_torque(v, true),
                BodyAction::SetLinvel(v) => body.set_linvel(v, true),
                BodyAction::SetAngvel(v) => body.set_angvel(v, true),
                BodyAction::Teleport(p, r) => body.set_position(p, r, true),
                BodyAction::WakeUp => body.wake_up(true),
                BodyAction::Sleep => body.sleep(),
            }
        }
    }

    pub(crate) fn set_native(&mut self, handle: Option<BodyHandle>) {
        self.native = handle;
    }

    pub(crate) fn read_back(&mut self, linvel: Vec3, angvel: Vec3, sleeping: bool) {
        self.linvel = linvel;
        self.angvel = angvel;
        self.sleeping = sleeping;
    }
}

/// 挂在节点上的碰撞体。
#[derive(Debug, Clone)]
pub struct Collider {
    desc: ColliderDesc,
    native: Option<ColliderHandle>,
    /// 上次绑定到的刚体。节点被改挂到别处时靠它发现「该重建了」。
    bound_to: Option<BodyHandle>,
    desc_dirty: bool,
    shape_dirty: bool,
}

impl Collider {
    /// 按描述创建。
    pub fn new(desc: ColliderDesc) -> Self {
        Self {
            desc,
            native: None,
            bound_to: None,
            desc_dirty: false,
            shape_dirty: false,
        }
    }

    /// 球形碰撞体。
    pub fn ball(radius: f32) -> Self {
        Self::new(ColliderDesc::ball(radius))
    }

    /// 盒形碰撞体，参数是半长。
    pub fn cuboid(half_extents: Vec3) -> Self {
        Self::new(ColliderDesc::cuboid(half_extents))
    }

    /// 沿 Y 的胶囊碰撞体。
    pub fn capsule_y(half_height: f32, radius: f32) -> Self {
        Self::new(ColliderDesc::capsule_y(half_height, radius))
    }

    /// 描述的只读引用。
    pub fn desc(&self) -> &ColliderDesc {
        &self.desc
    }

    /// 描述的可变引用，改完下次同步生效。
    pub fn desc_mut(&mut self) -> &mut ColliderDesc {
        self.desc_dirty = true;
        &mut self.desc
    }

    /// 换一个形状。
    pub fn set_shape(&mut self, shape: ColliderShape) {
        self.desc.shape = shape;
        self.shape_dirty = true;
    }

    /// 是否是传感器。
    pub fn is_sensor(&self) -> bool {
        self.desc.is_sensor
    }

    /// 切换传感器状态。
    pub fn set_sensor(&mut self, is_sensor: bool) {
        if self.desc.is_sensor != is_sensor {
            self.desc.is_sensor = is_sensor;
            self.desc_dirty = true;
        }
    }

    /// 对应的原生碰撞体句柄。
    pub fn native(&self) -> Option<ColliderHandle> {
        self.native
    }

    pub(crate) fn native_mut(&mut self) -> &mut Option<ColliderHandle> {
        &mut self.native
    }

    pub(crate) fn bound_to(&self) -> Option<BodyHandle> {
        self.bound_to
    }

    pub(crate) fn set_bound_to(&mut self, body: Option<BodyHandle>) {
        self.bound_to = body;
    }

    pub(crate) fn take_desc_dirty(&mut self) -> bool {
        std::mem::take(&mut self.desc_dirty)
    }

    pub(crate) fn take_shape_dirty(&mut self) -> bool {
        std::mem::take(&mut self.shape_dirty)
    }

    pub(crate) fn desc_ref(&self) -> &ColliderDesc {
        &self.desc
    }
}

/// 挂在节点上的关节，把两个带刚体的节点连起来。
#[derive(Debug, Clone)]
pub struct Joint {
    desc: JointDesc,
    body1: Handle<Node>,
    body2: Handle<Node>,
    native: Option<JointHandle>,
    /// 描述或两端刚体变了，需要重建原生关节。
    ///
    /// 关节没有「原地改参数」的路径——rapier 的 `GenericJoint` 是值语义，
    /// 改了就是换一个，重建比逐字段同步更简单也更不容易漏。
    dirty: bool,
}

impl Joint {
    /// 把 `body1` 与 `body2` 两个节点上的刚体连起来。
    pub fn new(body1: Handle<Node>, body2: Handle<Node>, desc: JointDesc) -> Self {
        Self {
            desc,
            body1,
            body2,
            native: None,
            dirty: false,
        }
    }

    /// 描述的只读引用。
    pub fn desc(&self) -> &JointDesc {
        &self.desc
    }

    /// 描述的可变引用。改完会在下次同步时重建原生关节。
    pub fn desc_mut(&mut self) -> &mut JointDesc {
        self.dirty = true;
        &mut self.desc
    }

    /// 第一端的节点。
    pub fn body1(&self) -> Handle<Node> {
        self.body1
    }

    /// 第二端的节点。
    pub fn body2(&self) -> Handle<Node> {
        self.body2
    }

    /// 换两端的节点。
    pub fn set_bodies(&mut self, body1: Handle<Node>, body2: Handle<Node>) {
        self.body1 = body1;
        self.body2 = body2;
        self.dirty = true;
    }

    /// 对应的原生关节句柄。
    pub fn native(&self) -> Option<JointHandle> {
        self.native
    }

    pub(crate) fn native_mut(&mut self) -> &mut Option<JointHandle> {
        &mut self.native
    }

    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub(crate) fn desc_ref(&self) -> &JointDesc {
        &self.desc
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn actions_survive_until_a_native_body_exists() {
        // 节点刚加进来、原生刚体还没建出来时调 apply_impulse，
        // 冲量必须留着，不能悄悄丢掉。
        let mut world = PhysicsWorld::new();
        let mut body = RigidBody::dynamic();
        body.apply_impulse(Vec3::X);

        body.flush(&mut world);
        assert_eq!(body.actions.len(), 1, "没有原生刚体时动作被吞掉了");

        let native = world.add_body(&RigidBodyDesc::dynamic(), 0);
        world
            .add_collider(&ColliderDesc::ball(0.5), Some(native), 0)
            .unwrap();
        body.set_native(Some(native));
        body.flush(&mut world);

        assert!(body.actions.is_empty(), "动作没有被执行");
        assert!(world.body(native).unwrap().linvel().x > 0.0);
    }

    #[test]
    fn changing_the_body_type_marks_the_desc_dirty_only_when_it_actually_changes() {
        let mut body = RigidBody::dynamic();
        body.set_body_type(RigidBodyType::Dynamic);
        assert!(!body.desc_dirty, "设成同一个值不该触发重推");

        body.set_body_type(RigidBodyType::Fixed);
        assert!(body.desc_dirty);
    }

    #[test]
    fn desc_mut_marks_dirty() {
        let mut body = RigidBody::dynamic();
        body.desc_mut().gravity_scale = 0.0;
        assert!(body.desc_dirty);
    }

    #[test]
    fn a_dirty_desc_is_pushed_to_the_native_body() {
        let mut world = PhysicsWorld::new();
        let native = world.add_body(&RigidBodyDesc::dynamic(), 0);
        let mut body = RigidBody::dynamic();
        body.set_native(Some(native));

        body.desc_mut().gravity_scale = 0.0;
        body.set_body_type(RigidBodyType::Fixed);
        body.flush(&mut world);

        assert_eq!(world.body(native).unwrap().body_type(), RigidBodyType::Fixed);
        assert!(!body.desc_dirty);
    }

    #[test]
    fn joint_desc_edits_request_a_rebuild() {
        let mut joint = Joint::new(
            Handle::NONE,
            Handle::NONE,
            JointDesc::fixed(Vec3::ZERO, Vec3::ZERO),
        );
        assert!(!joint.take_dirty());

        joint.desc_mut().local_anchor1 = Vec3::X;
        assert!(joint.take_dirty());
        assert!(!joint.take_dirty(), "脏标记该被取走一次就清掉");
    }

    #[test]
    fn setting_the_same_sensor_flag_is_a_no_op() {
        let mut collider = Collider::ball(1.0);
        collider.set_sensor(false);
        assert!(!collider.take_desc_dirty());

        collider.set_sensor(true);
        assert!(collider.take_desc_dirty());
    }
}

#[cfg(test)]
mod scene_test {
    use super::*;
    use crate::{ParticleSystem, Scene, Transform};
    use kmath::Mat4;
    use kphysics::{InteractionGroups, JointKind, RayCastOptions};

    /// 一块 100×1×100 的地面，上表面在 y = 0。
    fn add_ground(scene: &mut Scene) -> Handle<Node> {
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(50.0, 0.5, 50.0))),
        )
    }

    fn add_ball(scene: &mut Scene, position: Vec3) -> Handle<Node> {
        scene.add_node(
            Node::new("ball")
                .with_position(position)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.5)),
        )
    }

    fn run(scene: &mut Scene, seconds: f32) {
        let dt = 1.0 / 60.0;
        for _ in 0..(seconds / dt) as usize {
            scene.step_physics(dt);
            scene.update();
        }
    }

    #[test]
    fn a_body_added_this_frame_reaches_physics_on_the_same_step() {
        // `step_physics` 排在 `update` 之前，只靠 `update` 收集索引的话，
        // 这一帧新加的刚体要等到下一帧才进得了物理世界。
        let mut scene = Scene::new();
        add_ground(&mut scene);
        add_ball(&mut scene, Vec3::Y * 5.0);

        scene.step_physics(1.0 / 60.0);

        assert_eq!(scene.physics().body_count(), 2);
        assert_eq!(scene.physics().collider_count(), 2);
    }

    #[test]
    fn a_dynamic_body_writes_its_pose_back_into_the_node() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 10.0);

        run(&mut scene, 3.0);

        let y = scene.try_get(ball).unwrap().transform.position.y;
        assert!((y - 0.5).abs() < 0.1, "球停在了 y = {y}");
    }

    #[test]
    fn a_static_body_is_driven_by_the_node_not_the_other_way_around() {
        let mut scene = Scene::new();
        let ground = add_ground(&mut scene);
        scene.step_physics(1.0 / 60.0);

        // 用射线量地面在哪：初始上表面在 y = 0。
        let probe = RayCastOptions::new(Vec3::Y * 20.0, Vec3::NEG_Y, 100.0);
        let before = scene.cast_ray(&probe).unwrap().point.y;
        assert!(before.abs() < 1e-3, "初始地面不在 y = 0：{before}");

        // 挪节点，碰撞体必须跟着走。
        scene.try_get_mut(ground).unwrap().transform.position = Vec3::new(0.0, 2.5, 0.0);
        scene.step_physics(1.0 / 60.0);

        let after = scene.cast_ray(&probe).unwrap().point.y;
        assert!((after - 3.0).abs() < 1e-3, "静态碰撞体没跟着节点走：{after}");
    }

    #[test]
    fn a_static_body_never_falls_under_gravity() {
        let mut scene = Scene::new();
        let ground = add_ground(&mut scene);
        run(&mut scene, 2.0);

        assert_eq!(
            scene.try_get(ground).unwrap().transform.position,
            Vec3::new(0.0, -0.5, 0.0)
        );
    }

    #[test]
    fn changing_a_dynamic_body_transform_directly_has_no_effect() {
        // 这是最容易误用的一处：动态刚体的位置由物理说了算，
        // 直接改 transform 会在下一次同步时被覆盖。
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        run(&mut scene, 0.5);

        scene.try_get_mut(ball).unwrap().transform.position = Vec3::new(20.0, 20.0, 20.0);
        run(&mut scene, 0.2);

        let p = scene.try_get(ball).unwrap().transform.position;
        assert!(p.x.abs() < 1.0 && p.y < 10.0, "直接改 transform 竟然生效了：{p:?}");
    }

    #[test]
    fn teleport_is_the_way_to_move_a_dynamic_body() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        run(&mut scene, 0.5);

        scene
            .try_get_mut(ball)
            .unwrap()
            .rigid_body_mut()
            .unwrap()
            .teleport(Vec3::new(10.0, 8.0, 0.0), Quat::IDENTITY);
        scene.step_physics(1.0 / 60.0);
        scene.update();

        let p = scene.try_get(ball).unwrap().transform.position;
        assert!((p.x - 10.0).abs() < 0.01, "瞬移没生效：{p:?}");
    }

    #[test]
    fn an_impulse_from_the_node_api_reaches_the_body() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        run(&mut scene, 0.5);

        scene
            .try_get_mut(ball)
            .unwrap()
            .rigid_body_mut()
            .unwrap()
            .apply_impulse(Vec3::X * 2.0);
        run(&mut scene, 0.5);

        assert!(scene.try_get(ball).unwrap().transform.position.x > 0.5);
    }

    #[test]
    fn an_impulse_works_on_the_very_frame_the_body_is_created() {
        // 曾经的一个真 bug：刚体先建好、冲量当场 flush，而碰撞体要到下一步
        // 才加上——那时刚体质量还是 0，而 rapier 的 `Δv = 冲量 × 质量倒数`
        // 在质量为 0 时倒数也是 0，**冲量被静默吞掉**，不报错也没有任何迹象。
        // 症状是「新生成的物体第一帧推不动」，第二帧起又正常。
        // 抛射物、爆炸击飞这类「生成即施力」的用法全中招。
        let mut scene = Scene::new();
        let ball = scene.add_node(
            Node::new("projectile")
                .with_rigid_body(RigidBody::new(
                    kphysics::RigidBodyDesc::dynamic().with_gravity_scale(0.0),
                ))
                .with_collider(Collider::ball(0.5)),
        );

        // 建好的当帧就施加冲量，中间不插任何一次 step。
        scene[ball].rigid_body_mut().unwrap().apply_impulse(Vec3::Y * 10.0);
        scene.step_physics(1.0 / 60.0);

        let mass = 4.0 / 3.0 * std::f32::consts::PI * 0.5f32.powi(3);
        let velocity = scene[ball].rigid_body().unwrap().linvel().y;
        assert!(
            (velocity - 10.0 / mass).abs() < 0.5,
            "冲量没生效或算错了：{velocity}，期望 {}",
            10.0 / mass
        );
    }

    #[test]
    fn velocities_are_read_back_onto_the_component() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 10.0);
        run(&mut scene, 0.5);

        let linvel = scene.try_get(ball).unwrap().rigid_body().unwrap().linvel();
        assert!(linvel.y < -3.0, "下落中的球速度却是 {linvel:?}");
    }

    #[test]
    fn a_collider_on_a_child_node_binds_to_the_ancestor_body() {
        // 复合物体的写法：刚体在父节点，几块碰撞体各占一个子节点。
        let mut scene = Scene::new();
        add_ground(&mut scene);

        let body = scene.add_node(
            Node::new("dumbbell")
                .with_position(Vec3::Y * 6.0)
                .with_rigid_body(RigidBody::dynamic()),
        );
        scene.add_node_with_parent(
            Node::new("left")
                .with_position(Vec3::X * -0.6)
                .with_collider(Collider::ball(0.3)),
            body,
        );
        scene.add_node_with_parent(
            Node::new("right")
                .with_position(Vec3::X * 0.6)
                .with_collider(Collider::ball(0.3)),
            body,
        );

        scene.step_physics(1.0 / 60.0);

        // 两个碰撞体都该挂在同一个刚体上，而不是各自成为静态几何。
        assert_eq!(scene.physics().body_count(), 2);
        assert_eq!(scene.physics().collider_count(), 3);
        let native = scene
            .try_get(body)
            .unwrap()
            .rigid_body()
            .unwrap()
            .native()
            .unwrap();
        assert_eq!(scene.physics().body_colliders(native).len(), 2);

        run(&mut scene, 3.0);
        let y = scene.try_get(body).unwrap().transform.position.y;
        assert!((y - 0.3).abs() < 0.15, "哑铃停在了 y = {y}");
    }

    #[test]
    fn a_collider_without_any_body_is_static_geometry() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("wall").with_collider(Collider::cuboid(Vec3::splat(1.0))));
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);

        run(&mut scene, 3.0);

        let y = scene.try_get(ball).unwrap().transform.position.y;
        assert!(y > 1.0, "球该停在墙上，实际落到了 {y}");
    }

    #[test]
    fn removing_a_node_removes_its_physics_objects() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        scene.step_physics(1.0 / 60.0);
        assert_eq!(scene.physics().body_count(), 2);

        scene.remove_node(ball);
        scene.step_physics(1.0 / 60.0);

        assert_eq!(scene.physics().body_count(), 1, "物理世界里留下了幽灵刚体");
        assert_eq!(scene.physics().collider_count(), 1);
    }

    #[test]
    fn a_joint_between_two_nodes_holds_them_together() {
        let mut scene = Scene::new();
        add_ground(&mut scene);

        let anchor = scene.add_node(
            Node::new("anchor")
                .with_position(Vec3::Y * 8.0)
                .with_rigid_body(RigidBody::fixed()),
        );
        let bob = scene.add_node(
            Node::new("bob")
                .with_position(Vec3::new(2.0, 8.0, 0.0))
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.3)),
        );
        scene.add_node(Node::new("rope").with_joint(Joint::new(
            anchor,
            bob,
            JointDesc {
                kind: JointKind::Spherical {
                    limits: Default::default(),
                },
                local_anchor1: Vec3::ZERO,
                local_anchor2: Vec3::new(-2.0, 0.0, 0.0),
                ..Default::default()
            },
        )));

        // 无阻尼的单摆会一直荡，某一瞬间的高度说明不了什么；
        // 要看的是「荡到过多低」和「绳长有没有变」这两个不变量。
        let mut lowest = f32::MAX;
        let mut worst_radius_error: f32 = 0.0;
        for _ in 0..240 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
            let p = scene.try_get(bob).unwrap().transform.position;
            lowest = lowest.min(p.y);
            worst_radius_error =
                worst_radius_error.max(((p - Vec3::Y * 8.0).length() - 2.0).abs());
        }

        assert!(worst_radius_error < 0.1, "绳长最多偏了 {worst_radius_error}");
        assert!(lowest < 6.5, "摆锤最低只荡到 {lowest}");
    }

    #[test]
    fn a_joint_waits_for_both_bodies_to_exist() {
        // 关节两端的刚体还没建出来时不该崩，也不该建出半个关节。
        let mut scene = Scene::new();
        let missing = Handle::new(999, 1);
        scene.add_node(Node::new("early").with_joint(Joint::new(
            missing,
            missing,
            JointDesc::fixed(Vec3::ZERO, Vec3::ZERO),
        )));

        scene.step_physics(1.0 / 60.0);
        assert_eq!(scene.physics().joint_count(), 0);
    }

    #[test]
    fn a_ray_resolves_back_to_the_scene_node_it_hit() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        scene.step_physics(1.0 / 60.0);

        let hit = scene
            .cast_ray(&RayCastOptions::new(Vec3::Y * 20.0, Vec3::NEG_Y, 100.0))
            .expect("射线什么都没打到");

        assert_eq!(hit.body_node, Some(ball));
        assert_eq!(hit.collider_node, Some(ball));
        assert!(hit.normal.y > 0.9);
    }

    #[test]
    fn a_ray_can_exclude_the_body_it_starts_inside() {
        let mut scene = Scene::new();
        let ground = add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        scene.step_physics(1.0 / 60.0);

        let native = scene
            .try_get(ball)
            .unwrap()
            .rigid_body()
            .unwrap()
            .native()
            .unwrap();
        let hit = scene
            .cast_ray(
                &RayCastOptions::new(Vec3::Y * 20.0, Vec3::NEG_Y, 100.0).excluding_body(native),
            )
            .expect("该继续打到地面");

        assert_eq!(hit.body_node, Some(ground));
    }

    #[test]
    fn sensors_report_collisions_as_node_handles() {
        let mut scene = Scene::new();
        let trigger = scene.add_node(
            Node::new("trigger")
                .with_position(Vec3::Y * 3.0)
                .with_collider(Collider::new(
                    ColliderDesc::cuboid(Vec3::splat(1.0))
                        .as_sensor()
                        .with_collision_events(),
                )),
        );
        let ball = scene.add_node(
            Node::new("ball")
                .with_position(Vec3::Y * 8.0)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::new(
                    ColliderDesc::ball(0.5).with_collision_events(),
                )),
        );

        let mut seen = None;
        for _ in 0..180 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
            for event in scene.collision_events() {
                if event.started {
                    seen = Some(scene.collision_nodes(event));
                }
            }
            if seen.is_some() {
                break;
            }
        }

        let (a, b) = seen.expect("球穿过传感器却没有事件");
        let pair = [a, b];
        assert!(pair.contains(&Some(trigger)), "事件里没有传感器节点");
        assert!(pair.contains(&Some(ball)), "事件里没有球节点");
    }

    #[test]
    fn interaction_groups_keep_two_objects_from_colliding() {
        let mut scene = Scene::new();
        // 地面只和第 1 组交互，球属于第 2 组 —— 互不理睬，球该穿过去。
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::new(
                    ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0))
                        .with_groups(InteractionGroups::new(0b01, 0b01)),
                )),
        );
        let ball = scene.add_node(
            Node::new("ball")
                .with_position(Vec3::Y * 5.0)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::new(
                    ColliderDesc::ball(0.5).with_groups(InteractionGroups::new(0b10, 0b10)),
                )),
        );

        run(&mut scene, 2.0);

        assert!(
            scene.try_get(ball).unwrap().transform.position.y < -1.0,
            "分组没起作用，球被地面挡住了"
        );
    }

    #[test]
    fn a_kinematic_body_is_driven_by_its_node_and_pushes_dynamic_ones() {
        let mut scene = Scene::new();
        let platform = scene.add_node(
            Node::new("platform")
                .with_rigid_body(RigidBody::kinematic())
                .with_collider(Collider::cuboid(Vec3::new(2.0, 0.5, 2.0))),
        );
        let crate_node = scene.add_node(
            Node::new("crate")
                .with_position(Vec3::Y * 1.0)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::cuboid(Vec3::splat(0.5))),
        );

        for i in 0..120 {
            scene.try_get_mut(platform).unwrap().transform.position =
                Vec3::Y * (i as f32 * (1.0 / 60.0));
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let platform_y = scene.try_get(platform).unwrap().transform.position.y;
        let crate_y = scene.try_get(crate_node).unwrap().transform.position.y;

        assert!((platform_y - 2.0).abs() < 0.05, "平台没走到位：{platform_y}");
        assert!(crate_y > platform_y, "箱子没被平台顶着：{crate_y} vs {platform_y}");
    }

    #[test]
    fn a_body_under_a_moving_parent_still_lands_correctly() {
        // 刚体的位姿是世界空间的，写回节点时要换算成相对父节点的。
        // 少换算这一步，父节点带偏移时物体会跑到别处去。
        let mut scene = Scene::new();
        add_ground(&mut scene);

        let pivot = scene.add_node(Node::new("pivot").with_position(Vec3::new(10.0, 0.0, 0.0)));
        let ball = scene.add_node_with_parent(
            Node::new("ball")
                .with_position(Vec3::Y * 6.0)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.5)),
            pivot,
        );

        run(&mut scene, 3.0);

        let world = scene.world_matrix(ball).w_axis.truncate();
        assert!((world.y - 0.5).abs() < 0.1, "世界空间落点不对：{world:?}");
        assert!((world.x - 10.0).abs() < 0.1, "球在 X 上漂了：{world:?}");
    }

    #[test]
    fn node_scale_survives_the_write_back() {
        // 物理不认识缩放，回写位姿时不能顺手把它抹成 1。
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = scene.add_node(
            Node::new("ball")
                .with_transform(Transform {
                    position: Vec3::Y * 5.0,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(2.0),
                })
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.5)),
        );

        run(&mut scene, 1.0);

        assert_eq!(scene.try_get(ball).unwrap().transform.scale, Vec3::splat(2.0));
    }

    #[test]
    fn switching_a_body_to_fixed_stops_it_in_place() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 10.0);
        run(&mut scene, 0.5);

        let frozen_at = scene.try_get(ball).unwrap().transform.position;
        scene
            .try_get_mut(ball)
            .unwrap()
            .rigid_body_mut()
            .unwrap()
            .set_body_type(RigidBodyType::Fixed);
        run(&mut scene, 1.0);

        let now = scene.try_get(ball).unwrap().transform.position;
        assert!((now - frozen_at).length() < 0.05, "冻住的球还在动：{frozen_at:?} → {now:?}");
    }

    #[test]
    fn changing_a_collider_shape_rebuilds_it() {
        let mut scene = Scene::new();
        add_ground(&mut scene);
        let ball = add_ball(&mut scene, Vec3::Y * 5.0);
        run(&mut scene, 3.0);

        // 换成一个大得多的球，静止高度应当跟着变。
        scene
            .try_get_mut(ball)
            .unwrap()
            .collider_mut()
            .unwrap()
            .set_shape(ColliderShape::ball(2.0));
        run(&mut scene, 2.0);

        let y = scene.try_get(ball).unwrap().transform.position.y;
        assert!((y - 2.0).abs() < 0.15, "换形状后球停在了 {y}");
        assert_eq!(scene.physics().collider_count(), 2, "重建时漏删了旧碰撞体");
    }


    #[test]
    fn particles_bounce_off_real_scene_geometry() {
        // 这是「粒子碰撞」真正的验收：撞的是物理世界里那块地，
        // 不是手写的一块平面。
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(20.0, 0.5, 20.0))),
        );

        let node = scene.add_node(
            Node::new("sparks")
                .with_position(Vec3::Y * 5.0)
                .with_particles(
                    ParticleSystem::new(
                        kparticle::Emitter::sphere(0.0)
                            .with_rate(0.0)
                            .with_speed((0.0, 0.0))
                            .with_lifetime((10.0, 10.0)),
                    )
                    .with_acceleration(Vec3::new(0.0, -10.0, 0.0))
                    .with_space(kparticle::Space::World)
                    .with_seed(3)
                    .with_collision(
                        kparticle::Collision::scene()
                            .with_response(kparticle::CollisionResponse::bouncy()),
                    ),
                ),
        );

        scene.update();
        scene.step_physics(1.0 / 60.0);
        if let Some(system) = scene[node].particles_mut() {
            system.burst(1, Mat4::from_translation(Vec3::Y * 5.0));
        }

        let mut lowest = f32::MAX;
        let mut peak_upward = f32::MIN;
        for _ in 0..180 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
            scene.tick_particles(1.0 / 60.0);

            let system = scene[node].particles().unwrap();
            if let Some(position) = system.positions().first() {
                lowest = lowest.min(position.y);
                peak_upward = peak_upward.max(system.velocities()[0].y);
            }
        }

        assert!(peak_upward > 3.0, "粒子没从地面弹起来：{peak_upward}");
        assert!(lowest > -1.0, "粒子穿过了地面：{lowest}");
    }

    #[test]
    fn particles_without_scene_collision_fall_straight_through() {
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(20.0, 0.5, 20.0))),
        );
        let node = scene.add_node(
            Node::new("sparks")
                .with_position(Vec3::Y * 5.0)
                .with_particles(
                    ParticleSystem::new(
                        kparticle::Emitter::sphere(0.0)
                            .with_rate(0.0)
                            .with_speed((0.0, 0.0))
                            .with_lifetime((10.0, 10.0)),
                    )
                    .with_acceleration(Vec3::new(0.0, -10.0, 0.0))
                    .with_space(kparticle::Space::World)
                    .with_seed(3),
                ),
        );

        scene.update();
        if let Some(system) = scene[node].particles_mut() {
            system.burst(1, Mat4::from_translation(Vec3::Y * 5.0));
        }

        for _ in 0..120 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
            scene.tick_particles(1.0 / 60.0);
        }

        let y = scene[node].particles().unwrap().positions()[0].y;
        assert!(y < -5.0, "没开碰撞的粒子该一路穿下去，实际停在 {y}");
    }

    #[test]
    fn particles_do_not_push_the_scene_around() {
        // 粒子是只受影响、不施加影响的一方：一场火花不该把箱子掀翻。
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(20.0, 0.5, 20.0))),
        );
        let crate_node = scene.add_node(
            Node::new("crate")
                .with_position(Vec3::Y * 0.5)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::cuboid(Vec3::splat(0.5))),
        );
        let node = scene.add_node(
            Node::new("sparks")
                .with_position(Vec3::Y * 5.0)
                .with_particles(
                    ParticleSystem::new(
                        kparticle::Emitter::sphere(0.1)
                            .with_rate(0.0)
                            .with_speed((0.0, 0.0))
                            .with_lifetime((10.0, 10.0)),
                    )
                    .with_acceleration(Vec3::new(0.0, -30.0, 0.0))
                    .with_space(kparticle::Space::World)
                    .with_seed(5)
                    .with_collision(kparticle::Collision::scene()),
                ),
        );

        scene.update();
        scene.step_physics(1.0 / 60.0);
        if let Some(system) = scene[node].particles_mut() {
            system.burst(256, Mat4::from_translation(Vec3::Y * 5.0));
        }

        for _ in 0..180 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
            scene.tick_particles(1.0 / 60.0);
        }

        let position = scene[crate_node].transform.position;
        assert!(
            (position.x.abs() < 0.05) && (position.z.abs() < 0.05),
            "箱子被粒子推跑了：{position:?}"
        );
    }

    #[test]
    fn simulation_through_the_scene_graph_is_deterministic() {
        fn run_once() -> Vec3 {
            let mut scene = Scene::new();
            add_ground(&mut scene);
            let ball = add_ball(&mut scene, Vec3::new(0.1, 6.0, -0.2));
            scene
                .try_get_mut(ball)
                .unwrap()
                .rigid_body_mut()
                .unwrap()
                .apply_impulse(Vec3::new(0.3, 0.0, 0.15));
            run(&mut scene, 4.0);
            scene.try_get(ball).unwrap().transform.position
        }

        assert_eq!(run_once(), run_once());
    }
}
