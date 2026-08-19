//! 物理世界：刚体 / 碰撞体 / 关节的容器，以及步进与查询的入口。

use crate::{
    BodyHandle, ColliderHandle, JointHandle,
    body::{BodyMut, BodyRef, RigidBodyDesc},
    collider::{ColliderDesc, ColliderMut, ColliderRef, ColliderShape, InteractionGroups},
    convert::{from_rv, to_rp, to_rv},
    events::{CollisionEvent, ContactForceEvent},
    joint::JointDesc,
    query::{PointProjection, RayCastOptions, RayHit, ShapeCastOptions, ShapeHit},
};
use kmath::Vec3;
use rapier3d::{
    geometry::CollisionEventFlags,
    pipeline as rp,
    prelude::{QueryFilter, Ray},
};
use std::{sync::mpsc, time::Duration, time::Instant};

/// 一次步进的耗时统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicsStats {
    /// 上一次 [`PhysicsWorld::step`] 的耗时。
    pub step_time: Duration,
    /// 上一次步进后世界里的刚体数量。
    pub body_count: usize,
    /// 上一次步进后世界里的碰撞体数量。
    pub collider_count: usize,
}

/// 求解器的行为参数。
///
/// 默认值直接沿用 rapier 的调校结果——这些数字是它按大量场景试出来的，
/// 没有具体理由不要动。真要调，先动 `solver_iterations`（换稳定性）
/// 或 `max_ccd_substeps`（换穿模率）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntegrationParameters {
    /// 约束求解的迭代次数。越大越硬，堆叠越不容易塌，代价是线性的。
    pub solver_iterations: usize,
    /// 单步内最多做几次 CCD 子步。
    pub max_ccd_substeps: usize,
    /// 允许的穿透深度。太小会抖，太大会看到明显的重叠。
    pub allowed_linear_error: f32,
    /// 接触预测距离。求解器提前多远开始处理即将发生的接触。
    pub prediction_distance: f32,
    /// 长度单位。场景尺度远离「1 单位 = 1 米」时调这个，
    /// 各种阈值会跟着一起缩放。
    pub length_unit: f32,
}

impl Default for IntegrationParameters {
    fn default() -> Self {
        let d = rapier3d::dynamics::IntegrationParameters::default();
        Self {
            solver_iterations: d.num_solver_iterations,
            max_ccd_substeps: d.max_ccd_substeps,
            allowed_linear_error: d.normalized_allowed_linear_error,
            prediction_distance: d.normalized_prediction_distance,
            length_unit: d.length_unit,
        }
    }
}

/// 物理世界。
///
/// 这是 rapier 在本引擎里唯一的出口：外面看到的全是 kmath 类型与本 crate 的句柄，
/// rapier 的类型一个都不外泄。换引擎时要改的代码全在这一层里。
pub struct PhysicsWorld {
    pub(crate) inner: rp::PhysicsWorld,
    enabled: bool,
    event_handler: rp::ChannelEventCollector,
    collision_rx: mpsc::Receiver<rapier3d::geometry::CollisionEvent>,
    contact_force_rx: mpsc::Receiver<rapier3d::geometry::ContactForceEvent>,
    collision_events: Vec<CollisionEvent>,
    contact_force_events: Vec<ContactForceEvent>,
    stats: PhysicsStats,
    /// 自上次步进以来有没有增删过刚体 / 碰撞体。
    query_structures_stale: bool,
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PhysicsWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsWorld")
            .field("enabled", &self.enabled)
            .field("gravity", &self.gravity())
            .field("bodies", &self.body_count())
            .field("colliders", &self.collider_count())
            .finish()
    }
}

impl PhysicsWorld {
    /// 建一个重力为 (0, -9.81, 0) 的空世界。
    pub fn new() -> Self {
        let (collision_tx, collision_rx) = mpsc::channel();
        let (force_tx, contact_force_rx) = mpsc::channel();

        Self {
            inner: rp::PhysicsWorld::new(),
            enabled: true,
            event_handler: rp::ChannelEventCollector::new(collision_tx, force_tx),
            collision_rx,
            contact_force_rx,
            collision_events: Vec::new(),
            contact_force_events: Vec::new(),
            stats: PhysicsStats::default(),
            query_structures_stale: false,
        }
    }

    /// 当前重力。
    pub fn gravity(&self) -> Vec3 {
        from_rv(self.inner.gravity)
    }

    /// 设置重力。
    pub fn set_gravity(&mut self, gravity: Vec3) {
        self.inner.gravity = to_rv(gravity);
    }

    /// 模拟是否启用。
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 启用 / 暂停模拟。暂停时 [`step`](Self::step) 直接返回，世界保持原样。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// 读取求解器参数。
    pub fn integration_parameters(&self) -> IntegrationParameters {
        let p = &self.inner.integration_parameters;
        IntegrationParameters {
            solver_iterations: p.num_solver_iterations,
            max_ccd_substeps: p.max_ccd_substeps,
            allowed_linear_error: p.normalized_allowed_linear_error,
            prediction_distance: p.normalized_prediction_distance,
            length_unit: p.length_unit,
        }
    }

    /// 设置求解器参数。
    pub fn set_integration_parameters(&mut self, params: IntegrationParameters) {
        let p = &mut self.inner.integration_parameters;
        p.num_solver_iterations = params.solver_iterations.max(1);
        p.max_ccd_substeps = params.max_ccd_substeps;
        p.normalized_allowed_linear_error = params.allowed_linear_error;
        p.normalized_prediction_distance = params.prediction_distance;
        p.length_unit = params.length_unit;
    }

    /// 上一次步进的统计。
    pub fn stats(&self) -> PhysicsStats {
        self.stats
    }

    /// 推进模拟 `dt` 秒。
    ///
    /// **`dt` 应当是定值。** 变长步长下同一个场景每次跑出的结果都不一样，
    /// 堆叠的箱子会莫名其妙地塌。上层（`kapp`）用累加器把真实帧间隔切成
    /// 固定长度的子步再喂进来。
    pub fn step(&mut self, dt: f32) {
        self.collision_events.clear();
        self.contact_force_events.clear();

        if !self.enabled {
            // 真正的暂停：连碰撞检测都不跑，世界完全冻结。
            // 通道仍要排空，否则事件会攒到下一次步进时一起爆出来。
            self.drain_events();
            return;
        }

        // `dt = 0` 走的仍是完整管线，只是不积分——碰撞检测与查询结构照常刷新。
        // Fyrox 冻结物理时用的也是这一手（`GraphUpdateSwitches::physics_dt`）。
        self.inner.integration_parameters.dt = dt.max(0.0);

        let start = Instant::now();
        self.inner.step_with_events(&(), &self.event_handler);
        self.stats.step_time = start.elapsed();
        self.stats.body_count = self.inner.bodies.len();
        self.stats.collider_count = self.inner.colliders.len();
        self.query_structures_stale = false;

        self.drain_events();
    }

    /// 刷新查询用的加速结构，但**不推进**模拟。
    ///
    /// 射线 / 扫掠 / 点查询走的是广相的 BVH，而 BVH 是在 [`step`](Self::step)
    /// 里维护的。刚建好世界还没步进过、或者刚加完一批碰撞体就想立刻查询时，
    /// 必须先调一次这个——否则查询会**静默返回空结果**，既不报错也不 panic，
    /// 是最难查的那种问题。
    ///
    /// 已经是最新的话直接返回，重复调用不花钱。
    pub fn update_query_structures(&mut self) {
        if !self.query_structures_stale {
            return;
        }
        // 步长为 0 的一步：碰撞检测跑完、BVH 更新完，但什么都不会动。
        self.inner.integration_parameters.dt = 0.0;
        self.inner.step_with_events(&(), &self.event_handler);
        self.query_structures_stale = false;
        self.drain_events();
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.collision_rx.try_recv() {
            let (c1, c2, flags, started) = match event {
                rapier3d::geometry::CollisionEvent::Started(a, b, f) => (a, b, f, true),
                rapier3d::geometry::CollisionEvent::Stopped(a, b, f) => (a, b, f, false),
            };
            // 「因为碰撞体被删掉才结束」的事件里，碰撞体已经不在集合里了，
            // 查不到用户数据是正常的，落回 0。
            self.collision_events.push(CollisionEvent {
                collider1: ColliderHandle(c1),
                collider2: ColliderHandle(c2),
                user_data1: self.inner.colliders.get(c1).map_or(0, |c| c.user_data),
                user_data2: self.inner.colliders.get(c2).map_or(0, |c| c.user_data),
                started,
                sensor: flags.contains(CollisionEventFlags::SENSOR),
                removed: flags.contains(CollisionEventFlags::REMOVED),
            });
        }

        while let Ok(event) = self.contact_force_rx.try_recv() {
            self.contact_force_events.push(ContactForceEvent {
                collider1: ColliderHandle(event.collider1),
                collider2: ColliderHandle(event.collider2),
                user_data1: self
                    .inner
                    .colliders
                    .get(event.collider1)
                    .map_or(0, |c| c.user_data),
                user_data2: self
                    .inner
                    .colliders
                    .get(event.collider2)
                    .map_or(0, |c| c.user_data),
                total_force: from_rv(event.total_force),
                total_force_magnitude: event.total_force_magnitude,
                max_force_direction: from_rv(event.max_force_direction),
                max_force_magnitude: event.max_force_magnitude,
            });
        }
    }

    /// 上一次步进产生的碰撞开始 / 结束事件。
    ///
    /// 只有开了 [`ColliderDesc::emit_collision_events`] 的碰撞体才会上报。
    /// 每次 [`step`](Self::step) 会清空重填，不取就没了。
    pub fn collision_events(&self) -> &[CollisionEvent] {
        &self.collision_events
    }

    /// 上一次步进产生的接触力事件。
    pub fn contact_force_events(&self) -> &[ContactForceEvent] {
        &self.contact_force_events
    }

    // ───────────────────────── 刚体 ─────────────────────────

    /// 加一个刚体。`user_data` 由调用方定义，会原样带在原生刚体上，
    /// 查询结果里可以拿到——kscene 用它存节点句柄。
    pub fn add_body(&mut self, desc: &RigidBodyDesc, user_data: u128) -> BodyHandle {
        self.query_structures_stale = true;
        BodyHandle(self.inner.insert_body(desc.build(user_data)))
    }

    /// 删一个刚体，连同挂在它身上的碰撞体与关节。
    pub fn remove_body(&mut self, handle: BodyHandle) {
        self.query_structures_stale = true;
        self.inner.remove_body(handle.0);
    }

    /// 刚体是否还存在。
    pub fn has_body(&self, handle: BodyHandle) -> bool {
        self.inner.bodies.get(handle.0).is_some()
    }

    /// 刚体的只读视图。
    pub fn body(&self, handle: BodyHandle) -> Option<BodyRef<'_>> {
        self.inner.bodies.get(handle.0).map(BodyRef)
    }

    /// 刚体的可变视图。
    pub fn body_mut(&mut self, handle: BodyHandle) -> Option<BodyMut<'_>> {
        self.inner.bodies.get_mut(handle.0).map(BodyMut)
    }

    /// 世界里的刚体数量。
    pub fn body_count(&self) -> usize {
        self.inner.bodies.len()
    }

    /// 挂在某个刚体上的碰撞体。
    pub fn body_colliders(&self, handle: BodyHandle) -> Vec<ColliderHandle> {
        self.inner
            .bodies
            .get(handle.0)
            .map(|b| b.colliders().iter().copied().map(ColliderHandle).collect())
            .unwrap_or_default()
    }

    // ───────────────────────── 碰撞体 ─────────────────────────

    /// 加一个碰撞体。
    ///
    /// `parent` 给了就挂到该刚体上，位姿相对刚体；给 `None` 就是一个
    /// 静止不动的独立碰撞体，位姿是世界空间的。
    ///
    /// 返回 `None` 表示形状构建失败（几何退化），详见
    /// [`ColliderShape::to_shared_shape`](crate::ColliderShape)。
    pub fn add_collider(
        &mut self,
        desc: &ColliderDesc,
        parent: Option<BodyHandle>,
        user_data: u128,
    ) -> Option<ColliderHandle> {
        let collider = desc.build(user_data)?;
        self.query_structures_stale = true;
        Some(ColliderHandle(
            self.inner.insert_collider(collider, parent.map(|h| h.0)),
        ))
    }

    /// 删一个碰撞体。
    pub fn remove_collider(&mut self, handle: ColliderHandle) {
        self.query_structures_stale = true;
        self.inner.remove_collider(handle.0);
    }

    /// 碰撞体是否还存在。
    pub fn has_collider(&self, handle: ColliderHandle) -> bool {
        self.inner.colliders.get(handle.0).is_some()
    }

    /// 碰撞体的只读视图。
    pub fn collider(&self, handle: ColliderHandle) -> Option<ColliderRef<'_>> {
        self.inner.colliders.get(handle.0).map(ColliderRef)
    }

    /// 碰撞体的可变视图。
    pub fn collider_mut(&mut self, handle: ColliderHandle) -> Option<ColliderMut<'_>> {
        self.inner.colliders.get_mut(handle.0).map(ColliderMut)
    }

    /// 碰撞体所属的刚体。
    pub fn collider_parent(&self, handle: ColliderHandle) -> Option<BodyHandle> {
        self.inner.colliders.get(handle.0)?.parent().map(BodyHandle)
    }

    /// 世界里的碰撞体数量。
    pub fn collider_count(&self) -> usize {
        self.inner.colliders.len()
    }

    // ───────────────────────── 关节 ─────────────────────────

    /// 用关节把两个刚体连起来。
    pub fn add_joint(
        &mut self,
        body1: BodyHandle,
        body2: BodyHandle,
        desc: &JointDesc,
    ) -> JointHandle {
        JointHandle(
            self.inner
                .insert_impulse_joint(body1.0, body2.0, desc.build()),
        )
    }

    /// 删一个关节。
    pub fn remove_joint(&mut self, handle: JointHandle) {
        self.inner.remove_impulse_joint(handle.0);
    }

    /// 关节是否还存在。
    pub fn has_joint(&self, handle: JointHandle) -> bool {
        self.inner.impulse_joints().any(|(h, _)| h == handle.0)
    }

    /// 世界里的关节数量。
    pub fn joint_count(&self) -> usize {
        self.inner.impulse_joints().count()
    }

    // ───────────────────────── 查询 ─────────────────────────

    fn query_filter(
        &self,
        groups: InteractionGroups,
        exclude_collider: Option<ColliderHandle>,
        exclude_body: Option<BodyHandle>,
    ) -> QueryFilter<'_> {
        let mut filter = QueryFilter::new().groups(groups.to_rapier());
        if let Some(c) = exclude_collider {
            filter = filter.exclude_collider(c.0);
        }
        if let Some(b) = exclude_body {
            filter = filter.exclude_rigid_body(b.0);
        }
        filter
    }

    fn user_data_of(
        &self,
        collider: rapier3d::geometry::ColliderHandle,
    ) -> (u128, Option<BodyHandle>, u128) {
        let Some(c) = self.inner.colliders.get(collider) else {
            return (0, None, 0);
        };
        let body = c.parent();
        let body_data = body
            .and_then(|b| self.inner.bodies.get(b))
            .map_or(0, |b| b.user_data);
        (c.user_data, body.map(BodyHandle), body_data)
    }

    /// 打一条射线，返回最近的命中。
    pub fn cast_ray(&self, opts: &RayCastOptions) -> Option<RayHit> {
        let direction = opts.direction.normalize_or_zero();
        if direction == Vec3::ZERO {
            return None;
        }

        let ray = Ray::new(to_rv(opts.origin), to_rv(direction));
        let filter = self.query_filter(opts.groups, opts.exclude_collider, opts.exclude_body);

        let (handle, hit) =
            self.inner
                .cast_ray_and_get_normal(&ray, opts.max_distance, opts.solid, filter)?;
        let (collider_user_data, body, body_user_data) = self.user_data_of(handle);

        Some(RayHit {
            collider: ColliderHandle(handle),
            body,
            collider_user_data,
            body_user_data,
            point: opts.origin + direction * hit.time_of_impact,
            normal: from_rv(hit.normal),
            distance: hit.time_of_impact,
        })
    }

    /// 打一条射线，收集**路径上的所有**命中，按距离从近到远排好。
    ///
    /// 结果写进 `out`（会先清空），复用同一个 `Vec` 可以避免每帧分配。
    pub fn cast_ray_all(&self, opts: &RayCastOptions, out: &mut Vec<RayHit>) {
        out.clear();

        let direction = opts.direction.normalize_or_zero();
        if direction == Vec3::ZERO {
            return;
        }

        let ray = Ray::new(to_rv(opts.origin), to_rv(direction));
        let filter = self.query_filter(opts.groups, opts.exclude_collider, opts.exclude_body);

        for (handle, collider, hit) in
            self.inner
                .intersect_ray(ray, opts.max_distance, opts.solid, filter)
        {
            let body = collider.parent();
            out.push(RayHit {
                collider: ColliderHandle(handle),
                body: body.map(BodyHandle),
                collider_user_data: collider.user_data,
                body_user_data: body
                    .and_then(|b| self.inner.bodies.get(b))
                    .map_or(0, |b| b.user_data),
                point: opts.origin + direction * hit.time_of_impact,
                normal: from_rv(hit.normal),
                distance: hit.time_of_impact,
            });
        }

        // 广相是按空间划分遍历的，出来的顺序与距离无关；调用方几乎总是想要有序的。
        out.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    }

    /// 把一个形状沿方向扫过去，返回第一个撞上的碰撞体。
    ///
    /// 比射线贵得多，但能回答射线答不出的问题：「这个角色往前走会不会撞墙」。
    pub fn cast_shape(&self, shape: &ColliderShape, opts: &ShapeCastOptions) -> Option<ShapeHit> {
        let shape = shape.to_shared_shape()?;
        let filter = self.query_filter(opts.groups, None, opts.exclude_body);

        let cast_options = rapier3d::parry::query::ShapeCastOptions {
            max_time_of_impact: opts.max_distance,
            stop_at_penetration: opts.stop_at_penetration,
            ..Default::default()
        };

        let (handle, hit) = self.inner.cast_shape(
            &to_rp(opts.position, opts.rotation),
            to_rv(opts.velocity),
            shape.as_ref(),
            cast_options,
            filter,
        )?;
        let (collider_user_data, body, body_user_data) = self.user_data_of(handle);

        Some(ShapeHit {
            collider: ColliderHandle(handle),
            body,
            collider_user_data,
            body_user_data,
            witness_on_shape: from_rv(hit.witness1),
            witness_on_collider: from_rv(hit.witness2),
            normal: from_rv(hit.normal2),
            distance: hit.time_of_impact,
        })
    }

    /// 找离 `point` 最近的碰撞体表面点。
    pub fn project_point(
        &self,
        point: Vec3,
        max_distance: f32,
        solid: bool,
        groups: InteractionGroups,
    ) -> Option<PointProjection> {
        let filter = self.query_filter(groups, None, None);
        let (handle, projection) =
            self.inner
                .project_point(to_rv(point), max_distance, solid, filter)?;
        let (collider_user_data, body, body_user_data) = self.user_data_of(handle);

        Some(PointProjection {
            collider: ColliderHandle(handle),
            body,
            collider_user_data,
            body_user_data,
            point: from_rv(projection.point),
            is_inside: projection.is_inside,
        })
    }

    /// 找所有包含 `point` 的碰撞体。
    pub fn colliders_at_point(
        &self,
        point: Vec3,
        groups: InteractionGroups,
        out: &mut Vec<ColliderHandle>,
    ) {
        out.clear();
        let filter = self.query_filter(groups, None, None);
        for (handle, _) in self.inner.intersect_point(to_rv(point), filter) {
            out.push(ColliderHandle(handle));
        }
    }

    /// 找所有与给定形状重叠的碰撞体。
    pub fn colliders_intersecting_shape(
        &self,
        shape: &ColliderShape,
        position: Vec3,
        rotation: kmath::Quat,
        groups: InteractionGroups,
        out: &mut Vec<ColliderHandle>,
    ) {
        out.clear();
        let Some(shape) = shape.to_shared_shape() else {
            return;
        };
        let filter = self.query_filter(groups, None, None);
        for (handle, _) in
            self.inner
                .intersect_shape(to_rp(position, rotation), shape.as_ref(), filter)
        {
            out.push(ColliderHandle(handle));
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{ColliderDesc, RigidBodyType};
    use kmath::Quat;

    /// 一个「地面 + 悬空的球」的最小场景，很多测试都从这里起步。
    fn ball_over_ground() -> (PhysicsWorld, BodyHandle) {
        let mut world = PhysicsWorld::new();

        let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)),
                Some(ground),
                0,
            )
            .unwrap();

        let ball = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(0.0, 10.0, 0.0)),
            1,
        );
        world
            .add_collider(&ColliderDesc::ball(0.5), Some(ball), 1)
            .unwrap();

        // 查询结构由步进维护，而这个场景刚建好还没步进过。
        world.update_query_structures();

        (world, ball)
    }

    fn step_for(world: &mut PhysicsWorld, seconds: f32) {
        let dt = 1.0 / 60.0;
        for _ in 0..(seconds / dt) as usize {
            world.step(dt);
        }
    }

    #[test]
    fn a_dropped_ball_lands_on_the_ground_and_stays() {
        let (mut world, ball) = ball_over_ground();
        step_for(&mut world, 3.0);

        let y = world.body(ball).unwrap().position().y;
        // 地面上表面在 0.5，球半径 0.5 —— 静止时球心应当在 1.0 附近。
        assert!((y - 1.0).abs() < 0.1, "球停在了 y = {y}");
    }

    #[test]
    fn free_fall_follows_the_analytic_solution() {
        // 半秒自由落体：Δy = ½gt² ≈ 1.23 m。数值积分会有偏差，但不该差一个量级。
        let mut world = PhysicsWorld::new();
        let ball = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(0.0, 100.0, 0.0)),
            0,
        );
        world
            .add_collider(&ColliderDesc::ball(0.5), Some(ball), 0)
            .unwrap();

        step_for(&mut world, 0.5);

        let drop = 100.0 - world.body(ball).unwrap().position().y;
        let expected = 0.5 * 9.81 * 0.5 * 0.5;
        assert!(
            (drop - expected).abs() < 0.15,
            "落了 {drop}，理论值 {expected}"
        );
    }

    #[test]
    fn gravity_scale_zero_makes_a_body_float() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(
            &RigidBodyDesc::dynamic()
                .with_position(Vec3::Y * 5.0)
                .with_gravity_scale(0.0)
                .with_can_sleep(false),
            0,
        );
        world
            .add_collider(&ColliderDesc::ball(0.5), Some(body), 0)
            .unwrap();

        step_for(&mut world, 1.0);

        assert!((world.body(body).unwrap().position().y - 5.0).abs() < 1e-3);
    }

    #[test]
    fn fixed_bodies_do_not_move() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(&RigidBodyDesc::fixed().with_position(Vec3::Y * 5.0), 0);
        world
            .add_collider(&ColliderDesc::ball(0.5), Some(body), 0)
            .unwrap();

        step_for(&mut world, 1.0);

        assert_eq!(world.body(body).unwrap().position(), Vec3::Y * 5.0);
    }

    #[test]
    fn an_impulse_changes_velocity_by_impulse_over_mass() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(
            &RigidBodyDesc::dynamic()
                .with_gravity_scale(0.0)
                .with_can_sleep(false),
            0,
        );
        // 密度 1、半径 0.5 的球，质量 = 4/3·π·r³ ≈ 0.5236。
        world
            .add_collider(&ColliderDesc::ball(0.5).with_density(1.0), Some(body), 0)
            .unwrap();
        world.step(1.0 / 60.0);

        let mass = world.body(body).unwrap().mass();
        world
            .body_mut(body)
            .unwrap()
            .apply_impulse(Vec3::X * mass * 3.0, true);
        world.step(1.0 / 60.0);

        let vx = world.body(body).unwrap().linvel().x;
        assert!((vx - 3.0).abs() < 1e-3, "冲量后的速度是 {vx}");
    }

    #[test]
    fn removing_a_body_takes_its_colliders_with_it() {
        let (mut world, ball) = ball_over_ground();
        assert_eq!(world.collider_count(), 2);

        world.remove_body(ball);

        assert!(!world.has_body(ball));
        assert_eq!(world.collider_count(), 1, "刚体没了，碰撞体却还在");
    }

    #[test]
    fn a_ray_hits_the_nearest_collider_first() {
        let (world, _) = ball_over_ground();

        // 从球上方往下打：先撞球（y=10 附近），不该直接穿到地面。
        let hit = world
            .cast_ray(&RayCastOptions::new(
                Vec3::new(0.0, 20.0, 0.0),
                Vec3::NEG_Y,
                100.0,
            ))
            .expect("射线什么都没打到");

        assert_eq!(hit.body_user_data, 1, "打到的不是球");
        assert!((hit.point.y - 10.5).abs() < 0.01, "命中点 {:?}", hit.point);
        assert!(
            hit.normal.y > 0.9,
            "球顶的法线该朝上，实际 {:?}",
            hit.normal
        );
    }

    #[test]
    fn excluding_a_body_makes_the_ray_pass_through_it() {
        let (world, ball) = ball_over_ground();

        let hit = world
            .cast_ray(
                &RayCastOptions::new(Vec3::new(0.0, 20.0, 0.0), Vec3::NEG_Y, 100.0)
                    .excluding_body(ball),
            )
            .expect("射线该继续打到地面");

        assert_eq!(hit.body_user_data, 0, "球没有被排除掉");
    }

    #[test]
    fn cast_ray_all_returns_every_hit_sorted_by_distance() {
        let (world, _) = ball_over_ground();
        let mut hits = Vec::new();

        world.cast_ray_all(
            &RayCastOptions::new(Vec3::new(0.0, 20.0, 0.0), Vec3::NEG_Y, 100.0),
            &mut hits,
        );

        assert_eq!(hits.len(), 2, "球和地面都该被打到");
        assert!(hits[0].distance <= hits[1].distance, "结果没有按距离排序");
        assert_eq!(hits[0].body_user_data, 1);
    }

    #[test]
    fn interaction_groups_filter_ray_hits() {
        let (world, _) = ball_over_ground();

        // 只和第 2 组交互，而场景里所有碰撞体都是默认的全组 —— 成员位对得上，
        // 但对方的过滤位里没有我们这一组，所以什么都打不到。
        let hit = world.cast_ray(
            &RayCastOptions::new(Vec3::new(0.0, 20.0, 0.0), Vec3::NEG_Y, 100.0)
                .with_groups(InteractionGroups::new(0b10, 0b10)),
        );

        assert!(hit.is_some(), "默认分组是全通的，这里应当能打到");
    }

    #[test]
    fn a_zero_direction_ray_hits_nothing_instead_of_producing_nan() {
        let (world, _) = ball_over_ground();
        let hit = world.cast_ray(&RayCastOptions::new(Vec3::Y * 20.0, Vec3::ZERO, 100.0));

        assert!(hit.is_none());
    }

    #[test]
    fn shape_cast_finds_the_ground_below() {
        let (world, _) = ball_over_ground();

        let hit = world
            .cast_shape(
                &ColliderShape::ball(0.5),
                &ShapeCastOptions {
                    position: Vec3::new(20.0, 5.0, 0.0),
                    velocity: Vec3::NEG_Y,
                    max_distance: 100.0,
                    ..Default::default()
                },
            )
            .expect("向下扫掠该撞到地面");

        // 地面上表面 y = 0.5，球半径 0.5，从 y = 5 落下要走 4.0。
        assert!(
            (hit.distance - 4.0).abs() < 0.05,
            "扫掠距离 {}",
            hit.distance
        );
    }

    #[test]
    fn point_projection_lands_on_the_ball_surface() {
        let (world, _) = ball_over_ground();

        let projection = world
            .project_point(
                Vec3::new(0.0, 12.0, 0.0),
                10.0,
                true,
                InteractionGroups::ALL,
            )
            .expect("球就在附近");

        assert_eq!(projection.body_user_data, 1);
        assert!((projection.point.y - 10.5).abs() < 0.01);
        assert!(!projection.is_inside);
    }

    #[test]
    fn sensors_report_collision_events_without_pushing_anything() {
        let mut world = PhysicsWorld::new();

        let sensor_body = world.add_body(&RigidBodyDesc::fixed().with_position(Vec3::Y * 5.0), 7);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::splat(1.0))
                    .as_sensor()
                    .with_collision_events(),
                Some(sensor_body),
                7,
            )
            .unwrap();

        let ball = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec3::Y * 10.0), 9);
        world
            .add_collider(
                &ColliderDesc::ball(0.5).with_collision_events(),
                Some(ball),
                9,
            )
            .unwrap();

        let mut saw_start = false;
        for _ in 0..180 {
            world.step(1.0 / 60.0);
            if world
                .collision_events()
                .iter()
                .any(|e| e.started && e.sensor && e.involves_user_data(7))
            {
                saw_start = true;
            }
        }

        assert!(saw_start, "球穿过传感器却没有事件");
        // 传感器不产生碰撞响应，球应该一路落到远低于传感器的地方。
        assert!(world.body(ball).unwrap().position().y < 0.0);
    }

    #[test]
    fn queries_return_nothing_until_the_acceleration_structure_is_built() {
        // 这是最容易踩的坑：没步进过就查询，什么都查不到，还不报错。
        // 把这个行为连同它的解药一起钉住。
        let mut world = PhysicsWorld::new();
        let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)),
                Some(ground),
                0,
            )
            .unwrap();

        let ray = RayCastOptions::new(Vec3::Y * 10.0, Vec3::NEG_Y, 100.0);
        assert!(world.cast_ray(&ray).is_none(), "BVH 还没建，本该查不到");

        world.update_query_structures();
        assert!(world.cast_ray(&ray).is_some(), "刷新后仍然查不到");
    }

    #[test]
    fn stepping_also_refreshes_the_query_structures() {
        let mut world = PhysicsWorld::new();
        let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)),
                Some(ground),
                0,
            )
            .unwrap();

        world.step(1.0 / 60.0);

        assert!(
            world
                .cast_ray(&RayCastOptions::new(Vec3::Y * 10.0, Vec3::NEG_Y, 100.0))
                .is_some()
        );
    }

    #[test]
    fn a_collider_added_after_a_step_needs_another_refresh() {
        // 放在地面（半长 50）之外，这样「查不到」只可能是新碰撞体的缘故。
        let (mut world, _) = ball_over_ground();
        let extra = world.add_body(
            &RigidBodyDesc::fixed().with_position(Vec3::new(200.0, 0.0, 0.0)),
            5,
        );
        world
            .add_collider(&ColliderDesc::ball(1.0), Some(extra), 5)
            .unwrap();

        let ray = RayCastOptions::new(Vec3::new(200.0, 3.0, 0.0), Vec3::NEG_Y, 5.0);
        assert!(world.cast_ray(&ray).is_none());

        world.update_query_structures();
        assert_eq!(world.cast_ray(&ray).unwrap().body_user_data, 5);
    }

    #[test]
    fn events_are_cleared_each_step() {
        let mut world = PhysicsWorld::new();
        world.step(1.0 / 60.0);
        assert!(world.collision_events().is_empty());
    }

    #[test]
    fn a_disabled_world_freezes_everything() {
        let (mut world, ball) = ball_over_ground();
        world.set_enabled(false);

        step_for(&mut world, 1.0);

        assert_eq!(
            world.body(ball).unwrap().position(),
            Vec3::new(0.0, 10.0, 0.0)
        );
    }

    #[test]
    fn a_zero_dt_step_does_nothing() {
        let (mut world, ball) = ball_over_ground();
        world.step(0.0);

        assert_eq!(
            world.body(ball).unwrap().position(),
            Vec3::new(0.0, 10.0, 0.0)
        );
    }

    #[test]
    fn locked_rotations_keep_a_capsule_upright() {
        // 角色胶囊的招牌需求：撞到斜坡也不能躺倒。
        let mut world = PhysicsWorld::new();
        let ground = world.add_body(
            &RigidBodyDesc::fixed().with_rotation(Quat::from_rotation_z(0.4)),
            0,
        );
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)),
                Some(ground),
                0,
            )
            .unwrap();

        let capsule = world.add_body(
            &RigidBodyDesc::dynamic()
                .with_position(Vec3::Y * 5.0)
                .with_locked_rotations(),
            1,
        );
        world
            .add_collider(&ColliderDesc::capsule_y(0.5, 0.3), Some(capsule), 1)
            .unwrap();

        step_for(&mut world, 2.0);

        let up = world.body(capsule).unwrap().rotation() * Vec3::Y;
        assert!(up.y > 0.999, "胶囊倒了，up = {up:?}");
    }

    #[test]
    fn kinematic_bodies_push_dynamic_ones_but_are_not_pushed_back() {
        let mut world = PhysicsWorld::new();

        let platform = world.add_body(&RigidBodyDesc::kinematic_position_based(), 0);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(1.0, 0.5, 1.0)),
                Some(platform),
                0,
            )
            .unwrap();

        let box_body = world.add_body(
            &RigidBodyDesc::dynamic()
                .with_position(Vec3::Y * 1.0)
                .with_gravity_scale(0.0)
                .with_can_sleep(false),
            1,
        );
        world
            .add_collider(&ColliderDesc::cuboid(Vec3::splat(0.5)), Some(box_body), 1)
            .unwrap();

        // 平台匀速上升，把箱子顶走。
        for i in 0..120 {
            let y = i as f32 * (1.0 / 60.0) * 1.0;
            world
                .body_mut(platform)
                .unwrap()
                .set_next_kinematic_position(Vec3::Y * y, Quat::IDENTITY);
            world.step(1.0 / 60.0);
        }

        let platform_y = world.body(platform).unwrap().position().y;
        let box_y = world.body(box_body).unwrap().position().y;

        assert!(
            (platform_y - 2.0).abs() < 0.05,
            "平台没走到位：{platform_y}"
        );
        assert!(box_y > 2.0, "箱子没被顶起来：{box_y}");
    }

    #[test]
    fn a_fixed_joint_welds_two_bodies_together() {
        use crate::JointDesc;

        let mut world = PhysicsWorld::new();
        let anchor = world.add_body(&RigidBodyDesc::fixed().with_position(Vec3::Y * 10.0), 0);
        let hanging = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(2.0, 10.0, 0.0)),
            1,
        );
        world
            .add_collider(&ColliderDesc::ball(0.3), Some(hanging), 1)
            .unwrap();

        world.add_joint(
            anchor,
            hanging,
            &JointDesc::fixed(Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO),
        );

        step_for(&mut world, 2.0);

        let p = world.body(hanging).unwrap().position();
        assert!(
            (p - Vec3::new(2.0, 10.0, 0.0)).length() < 0.05,
            "被焊住的球跑到了 {p:?}"
        );
    }

    #[test]
    fn a_revolute_joint_lets_a_pendulum_swing_around_its_axis_only() {
        use crate::JointDesc;

        let mut world = PhysicsWorld::new();
        let pivot = world.add_body(&RigidBodyDesc::fixed().with_position(Vec3::Y * 10.0), 0);
        let bob = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(2.0, 10.0, 0.0)),
            1,
        );
        world
            .add_collider(&ColliderDesc::ball(0.3), Some(bob), 1)
            .unwrap();

        // 绕 Z 轴的铰链：摆锤只能在 XY 平面里荡。
        world.add_joint(
            pivot,
            bob,
            &JointDesc::revolute(Vec3::ZERO, Vec3::new(-2.0, 0.0, 0.0), Vec3::Z, None),
        );

        step_for(&mut world, 1.0);

        let p = world.body(bob).unwrap().position();
        assert!(p.y < 9.9, "摆锤没有荡下来：{p:?}");
        assert!(p.z.abs() < 0.05, "摆锤跑出了 XY 平面：{p:?}");
        // 到转轴的距离恒定，这是铰链的定义。
        let radius = (p - Vec3::Y * 10.0).length();
        assert!((radius - 2.0).abs() < 0.05, "摆长变成了 {radius}");
    }

    #[test]
    fn body_type_can_be_switched_at_runtime() {
        // 布娃娃切换就靠这个：运动学（跟动画）↔ 动态（受物理）。
        let (mut world, ball) = ball_over_ground();
        world
            .body_mut(ball)
            .unwrap()
            .set_body_type(RigidBodyType::KinematicPositionBased, true);

        step_for(&mut world, 1.0);
        assert_eq!(world.body(ball).unwrap().position().y, 10.0);

        world
            .body_mut(ball)
            .unwrap()
            .set_body_type(RigidBodyType::Dynamic, true);
        step_for(&mut world, 1.0);
        assert!(world.body(ball).unwrap().position().y < 10.0);
    }

    #[test]
    fn stats_track_the_world_contents() {
        let (mut world, _) = ball_over_ground();
        world.step(1.0 / 60.0);

        assert_eq!(world.stats().body_count, 2);
        assert_eq!(world.stats().collider_count, 2);
    }

    #[test]
    fn degenerate_shapes_fail_to_add_instead_of_panicking() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(&RigidBodyDesc::dynamic(), 0);
        let desc = ColliderDesc::new(ColliderShape::ConvexHull(std::sync::Arc::new(vec![
            Vec3::ZERO,
        ])));

        assert!(world.add_collider(&desc, Some(body), 0).is_none());
        assert_eq!(world.collider_count(), 0);
    }

    #[test]
    fn simulation_is_deterministic_for_identical_inputs() {
        // 同样的场景、同样的步长，两次跑必须逐位一致 —— 否则物理没法做回归测试。
        fn run() -> Vec3 {
            let (mut world, ball) = ball_over_ground();
            world
                .body_mut(ball)
                .unwrap()
                .apply_impulse(Vec3::new(0.3, 0.0, -0.2), true);
            for _ in 0..300 {
                world.step(1.0 / 60.0);
            }
            world.body(ball).unwrap().position()
        }

        assert_eq!(run(), run());
    }
}
