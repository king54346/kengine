//! 2D 物理世界。

use super::{
    body::{BodyMut, BodyRef, RigidBodyDesc},
    collider::{ColliderDesc, ColliderMut, ColliderRef},
    convert::{from_rv, to_rv},
};
use crate::IntegrationParameters;
use kmath::Vec2;
use rapier2d::{
    geometry::CollisionEventFlags,
    pipeline as rp,
    prelude::{QueryFilter, Ray},
};
use std::sync::mpsc;

/// 2D 刚体的句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BodyHandle(pub(crate) rapier2d::dynamics::RigidBodyHandle);

/// 2D 碰撞体的句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColliderHandle(pub(crate) rapier2d::geometry::ColliderHandle);

/// 一次射线检测的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// 命中的碰撞体。
    pub collider: ColliderHandle,
    /// 命中点。
    pub point: Vec2,
    /// 命中处的表面法线。
    pub normal: Vec2,
    /// 沿射线走了多远。
    pub time_of_impact: f32,
}

/// 射线检测的参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayCastOptions {
    /// 起点。
    pub origin: Vec2,
    /// 方向，不必归一化——`time_of_impact` 是以它为单位量的。
    pub direction: Vec2,
    /// 最远走多少个 `direction`。
    pub max_distance: f32,
    /// 是否把碰撞体当作实心。
    ///
    /// 为真时，起点落在形状**内部**的射线会立刻命中、距离为 0；
    /// 为假时射线会从内部穿出去、命中背面。做「脚下有没有地面」
    /// 这类检测要用实心，否则角色陷进地里时会检测不到地面。
    pub solid: bool,
    /// 过滤：只检测这些组。
    pub groups: crate::InteractionGroups,
}

impl Default for RayCastOptions {
    fn default() -> Self {
        Self {
            origin: Vec2::ZERO,
            direction: Vec2::new(0.0, -1.0),
            max_distance: f32::MAX,
            solid: true,
            groups: crate::InteractionGroups::ALL,
        }
    }
}

/// 2D 物理世界。
///
/// 和 3D 的 [`PhysicsWorld`](crate::PhysicsWorld) 是两个**独立**的世界，
/// 互不感知。同一个游戏里两个都用是可以的（3D 场景 + 2D 小游戏），
/// 但一个 2D 刚体永远不会撞到一个 3D 刚体。
pub struct PhysicsWorld {
    pub(super) inner: rp::PhysicsWorld,
    enabled: bool,
    event_handler: rp::ChannelEventCollector,
    collision_rx: mpsc::Receiver<rapier2d::geometry::CollisionEvent>,
    contact_force_rx: mpsc::Receiver<rapier2d::geometry::ContactForceEvent>,
    collision_events: Vec<CollisionEvent2d>,
    contact_force_events: Vec<ContactForceEvent2d>,
    /// 自上次步进以来有没有增删过刚体 / 碰撞体。
    query_structures_stale: bool,
}

/// 2D 的碰撞事件。
///
/// 和 3D 的 [`CollisionEvent`] 结构相同，但句柄类型不同——
/// 混用会在编译期被拦下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollisionEvent2d {
    /// 参与碰撞的两个碰撞体之一。
    pub first: ColliderHandle,
    /// 另一个。
    pub second: ColliderHandle,
    /// 是开始接触还是结束接触。
    pub started: bool,
    /// 至少一方是传感器。
    pub sensor: bool,
}

/// 2D 的接触力事件。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactForceEvent2d {
    /// 参与接触的两个碰撞体之一。
    pub first: ColliderHandle,
    /// 另一个。
    pub second: ColliderHandle,
    /// 合力的大小。
    pub total_force_magnitude: f32,
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PhysicsWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsWorld2d")
            .field("enabled", &self.enabled)
            .field("gravity", &self.gravity())
            .field("bodies", &self.body_count())
            .field("colliders", &self.collider_count())
            .finish()
    }
}

impl PhysicsWorld {
    /// 建一个重力为 (0, -9.81) 的空世界。
    ///
    /// 重力朝 **-Y**：2D 里 Y 轴朝上是数学惯例，也和引擎的精灵坐标一致。
    pub fn new() -> Self {
        let (collision_tx, collision_rx) = mpsc::channel();
        let (force_tx, contact_force_rx) = mpsc::channel();
        let inner = rp::PhysicsWorld {
            gravity: to_rv(Vec2::new(0.0, -9.81)),
            ..Default::default()
        };

        Self {
            inner,
            enabled: true,
            event_handler: rp::ChannelEventCollector::new(collision_tx, force_tx),
            collision_rx,
            contact_force_rx,
            collision_events: Vec::new(),
            contact_force_events: Vec::new(),
            query_structures_stale: false,
        }
    }

    /// 重力。
    pub fn gravity(&self) -> Vec2 {
        from_rv(self.inner.gravity)
    }

    /// 设置重力。
    pub fn set_gravity(&mut self, gravity: Vec2) {
        self.inner.gravity = to_rv(gravity);
    }

    /// 模拟是否启用。关掉后 [`step`](Self::step) 什么都不做。
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 启用 / 禁用模拟。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// 求解器参数。
    pub fn integration_parameters(&self) -> IntegrationParameters {
        IntegrationParameters {
            solver_iterations: self.inner.integration_parameters.num_solver_iterations,
            max_ccd_substeps: self.inner.integration_parameters.max_ccd_substeps,
            allowed_linear_error: self
                .inner
                .integration_parameters
                .normalized_allowed_linear_error,
            prediction_distance: self
                .inner
                .integration_parameters
                .normalized_prediction_distance,
            length_unit: self.inner.integration_parameters.length_unit,
        }
    }

    /// 设置求解器参数。
    pub fn set_integration_parameters(&mut self, parameters: IntegrationParameters) {
        let p = &mut self.inner.integration_parameters;
        p.num_solver_iterations = parameters.solver_iterations;
        p.max_ccd_substeps = parameters.max_ccd_substeps;
        p.normalized_allowed_linear_error = parameters.allowed_linear_error;
        p.normalized_prediction_distance = parameters.prediction_distance;
        p.length_unit = parameters.length_unit;
    }

    /// 世界里的刚体数量。
    pub fn body_count(&self) -> usize {
        self.inner.bodies.len()
    }

    /// 世界里的碰撞体数量。
    pub fn collider_count(&self) -> usize {
        self.inner.colliders.len()
    }

    /// 加一个刚体。`user_data` 由调用方定义，`kscene` 往里放节点句柄。
    pub fn add_body(&mut self, desc: &RigidBodyDesc, user_data: u128) -> BodyHandle {
        self.query_structures_stale = true;
        BodyHandle(self.inner.bodies.insert(desc.build(user_data)))
    }

    /// 加一个碰撞体，可选地挂到某个刚体上。
    ///
    /// 形状退化（共线的凸包、只有一个点的折线）时返回 [`None`]——
    /// 把退化形状塞给求解器会算出 NaN，然后整个世界飞出去。
    pub fn add_collider(
        &mut self,
        desc: &ColliderDesc,
        parent: Option<BodyHandle>,
        user_data: u128,
    ) -> Option<ColliderHandle> {
        let collider = desc.build(user_data)?;
        self.query_structures_stale = true;
        let handle = match parent {
            Some(body) => {
                self.inner
                    .colliders
                    .insert_with_parent(collider, body.0, &mut self.inner.bodies)
            }
            None => self.inner.colliders.insert(collider),
        };
        Some(ColliderHandle(handle))
    }

    /// 删一个刚体，连同它的碰撞体和关节。
    pub fn remove_body(&mut self, handle: BodyHandle) {
        self.query_structures_stale = true;
        self.inner.bodies.remove(
            handle.0,
            &mut self.inner.islands,
            &mut self.inner.colliders,
            &mut self.inner.impulse_joints,
            &mut self.inner.multibody_joints,
            // true：连同碰撞体一起删。留着的话它们会变成没有归属的
            // 幽灵碰撞体，仍然参与检测。
            true,
        );
    }

    /// 删一个碰撞体。
    pub fn remove_collider(&mut self, handle: ColliderHandle) {
        self.query_structures_stale = true;
        self.inner.colliders.remove(
            handle.0,
            &mut self.inner.islands,
            &mut self.inner.bodies,
            // true：让所属刚体重算质量。不重算的话删掉一个碰撞体
            // 之后刚体还带着它的质量。
            true,
        );
    }

    /// 只读地看一个刚体。
    pub fn body(&self, handle: BodyHandle) -> Option<BodyRef<'_>> {
        self.inner
            .bodies
            .get(handle.0)
            .map(|inner| BodyRef { inner })
    }

    /// 可写地操作一个刚体。
    pub fn body_mut(&mut self, handle: BodyHandle) -> Option<BodyMut<'_>> {
        self.inner
            .bodies
            .get_mut(handle.0)
            .map(|inner| BodyMut { inner })
    }

    /// 只读地看一个碰撞体。
    pub fn collider(&self, handle: ColliderHandle) -> Option<ColliderRef<'_>> {
        self.inner
            .colliders
            .get(handle.0)
            .map(|inner| ColliderRef { inner })
    }

    /// 可写地操作一个碰撞体。
    pub fn collider_mut(&mut self, handle: ColliderHandle) -> Option<ColliderMut<'_>> {
        self.inner
            .colliders
            .get_mut(handle.0)
            .map(|inner| ColliderMut { inner })
    }

    /// 遍历所有刚体的句柄。
    pub fn body_handles(&self) -> Vec<BodyHandle> {
        self.inner
            .bodies
            .iter()
            .map(|(h, _)| BodyHandle(h))
            .collect()
    }

    /// 步进一次。
    pub fn step(&mut self, dt: f32) {
        self.collision_events.clear();
        self.contact_force_events.clear();
        if !self.enabled || dt <= 0.0 {
            // dt <= 0 时直接返回：负的时间步会让积分器往回走，
            // 0 会让一堆除法变成除以零。
            return;
        }

        self.inner.integration_parameters.dt = dt;
        self.inner.step_with_events(&(), &self.event_handler);
        self.query_structures_stale = false;

        self.drain_events();
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.collision_rx.try_recv() {
            let (first, second, started, flags) = match event {
                rapier2d::geometry::CollisionEvent::Started(a, b, flags) => (a, b, true, flags),
                rapier2d::geometry::CollisionEvent::Stopped(a, b, flags) => (a, b, false, flags),
            };
            self.collision_events.push(CollisionEvent2d {
                first: ColliderHandle(first),
                second: ColliderHandle(second),
                started,
                sensor: flags.contains(CollisionEventFlags::SENSOR),
            });
        }
        while let Ok(event) = self.contact_force_rx.try_recv() {
            self.contact_force_events.push(ContactForceEvent2d {
                first: ColliderHandle(event.collider1),
                second: ColliderHandle(event.collider2),
                total_force_magnitude: event.total_force_magnitude,
            });
        }
    }

    /// 上一次步进产生的碰撞事件。
    pub fn collision_events(&self) -> &[CollisionEvent2d] {
        &self.collision_events
    }

    /// 上一次步进产生的接触力事件。
    pub fn contact_force_events(&self) -> &[ContactForceEvent2d] {
        &self.contact_force_events
    }

    /// 射线检测，返回最近的一次命中。
    pub fn cast_ray(&mut self, options: &RayCastOptions) -> Option<RayHit> {
        // 查询结构在步进时才更新。刚加完刚体就查询的话，查询树里
        // 还没有它——这在「加载关卡后立刻检测地面」时会静默返回 None。
        self.update_query_structures();

        let ray = Ray::new(to_rv(options.origin), to_rv(options.direction));
        let filter = QueryFilter::default().groups(options.groups.to_rapier2d());

        let (handle, intersection) = self
            .inner
            .query_pipeline_with_filter(filter)
            .cast_ray_and_get_normal(&ray, options.max_distance, options.solid)?;

        Some(RayHit {
            collider: ColliderHandle(handle),
            point: from_rv(ray.point_at(intersection.time_of_impact)),
            normal: from_rv(intersection.normal),
            time_of_impact: intersection.time_of_impact,
        })
    }

    /// 一个点落在哪些碰撞体里。
    pub fn colliders_at_point(&mut self, point: Vec2) -> Vec<ColliderHandle> {
        self.update_query_structures();
        self.inner
            .query_pipeline()
            .intersect_point(to_rv(point))
            .map(|(handle, _)| ColliderHandle(handle))
            .collect()
    }

    /// 让射线 / 点查询用的加速结构跟上最新的增删。
    ///
    /// 和 3D 的 [`update_query_structures`](crate::PhysicsWorld::update_query_structures)
    /// 同理：查询走的是广相的 BVH，而 BVH 是在 [`step`](Self::step) 里维护的。
    /// 刚加完碰撞体就查询的话，查询会**静默返回空结果**——既不报错也不 panic。
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
}
