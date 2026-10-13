//! 真切割碎裂：用一个世界空间平面把凸刚体切成两块。
//!
//! 之前示例里的「碎裂」是删一加八——碎块全是小盒子，与撞击方向无关。
//! 这里做的是真切割：一个过撞击点、包含撞击方向的平面（[`FracturePlane::from_impact`]），
//! 母体两侧的几何各自重建凸包，生成两个带速度继承的子刚体。
//!
//! # 支持的母体形状
//!
//! - 立方体（`Cuboid`）：8 个角点、12 条棱都是已知的，切割是精确的——
//!   跨过平面的棱与平面求交，切割面顶点会补进两侧点集，
//!   任意穿过内部的平面都切得出两块（除非本来就是擦边）。
//! - 凸包（`ConvexHull`）：只有点集没有拓扑，切割面顶点算不出，
//!   退化成「点集按符号分边 + 各自重建凸包」的近似。切面会内凹一点，
//!   偏心撞击更容易因点数不够被拒。
//!
//! 其他形状（球 / 胶囊 / 网格 / 高度场……）直接返回 `None`，
//! 调用方应回落到旧的八等分或干脆不碎。球被平面切的解析形状
//! 不是凸多面体，硬套凸包会凭空多出体积，不如不切。
//!
//! # 失败时世界不变
//!
//! [`PhysicsWorld::fracture_collider`] 在删母体之前先把两块的几何、
//! 描述全算好，任何一步失败都直接返回 `None`，世界里什么都没动。
//! 调用方无需做回滚。

use crate::{
    BodyHandle, ColliderHandle,
    body::RigidBodyDesc,
    collider::{CoefficientCombineRule, ColliderDesc, ColliderShape, InteractionGroups},
    convert::{from_rv, to_rv},
};
use kmath::{Quat, Vec3};
use std::sync::Arc;

/// 切割平面，世界空间。
///
/// 法线不需要事先归一化，[`FracturePlane::new`] 会处理；
/// 零向量归一化不出来，返回 `None`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FracturePlane {
    /// 平面上的一点，通常是撞击点。
    pub point: Vec3,
    /// 平面法线，指向「正侧」。
    pub normal: Vec3,
}

impl FracturePlane {
    /// 构造切割平面。法线接近零向量时返回 `None`。
    pub fn new(point: Vec3, normal: Vec3) -> Option<Self> {
        let normal = normal.try_normalize()?;
        if !normal.is_finite() {
            return None;
        }
        Some(Self { point, normal })
    }

    /// 撞击裂出来的平面：过撞击点、**包含**撞击方向（裂缝顺着冲击劈进去）。
    ///
    /// 别拿撞击方向直接当法线：接触力的方向就是被撞那个面的法线，撞击点又在那个面上，
    /// 那样的平面和被撞的面重合，切出来是擦边薄片，[`PhysicsWorld::fracture_collider`] 会拒掉。
    /// 方向退化（零向量）时返回 `None`。
    ///
    /// [`PhysicsWorld::fracture_collider`]: crate::PhysicsWorld::fracture_collider
    pub fn from_impact(point: Vec3, direction: Vec3) -> Option<Self> {
        let direction = direction.try_normalize()?;
        Self::new(point, impact_crack_normal(direction))
    }

    /// 点到平面的有符号距离，正的在法线一侧。
    fn signed_distance(&self, p: Vec3) -> f32 {
        (p - self.point).dot(self.normal)
    }
}

/// 撞击裂缝平面的法线：和撞击方向垂直，于是平面包含撞击方向。
///
/// 包含一个方向的平面有无数个（绕它转一圈），取竖着的那个——法线也垂直于 Y，碎块左右分开；
/// 撞击本身是竖直的（从上往下砸）就换成和 X 垂直。返回单位向量；方向退化时按 +Z 算。
pub fn impact_crack_normal(direction: Vec3) -> Vec3 {
    let direction = direction.normalize_or(Vec3::Z);
    let normal = direction.cross(Vec3::Y);
    if normal.length_squared() > 1e-4 {
        normal.normalize()
    } else {
        direction.cross(Vec3::X).normalize()
    }
}

/// 碎裂选项。
#[derive(Debug, Clone, PartialEq)]
pub struct FractureOptions {
    /// 两块沿平面法线弹开的初速（米/秒），叠加在继承的母体速度上。
    /// 不给这个两块会粘在一起，下一帧就被求解器当穿透弹开，
    /// 看起来像炸——给个 0.5 左右的分离速度体面得多。
    pub separation_kick: f32,
    /// 子刚体的用户数据，默认都继承母体的。
    ///
    /// 注意 kscene 用刚体的用户数据反查场景节点，两块拿同一个值
    /// 会查到同一个节点——场景侧用完必须立刻改掉其中一个。
    pub child_user_data: Option<[u128; 2]>,
    /// 子碰撞体的用户数据，默认都继承母碰撞体的。同上，用完即改。
    pub child_collider_user_data: Option<[u128; 2]>,
    /// 子碰撞体的接触力上报阈值（牛顿）。负值继承母体的阈值；
    /// 非负值显式指定（0 表示不上报，碎块不再碎——限代靠它）。
    /// 想要「碎块再碎」就显式给一个正值。
    pub child_contact_threshold: f32,
    /// 碎块沿法线方向的最小厚度（米）。比这还薄的片会被判成擦边，
    /// 整个碎裂直接取消——碎屑除了费求解器没有意义。
    pub min_thickness: f32,
}

impl Default for FractureOptions {
    fn default() -> Self {
        Self {
            separation_kick: 0.5,
            child_user_data: None,
            child_collider_user_data: None,
            child_contact_threshold: -1.0,
            min_thickness: 1e-3,
        }
    }
}

/// 碎块的渲染几何，子刚体局部空间（原点在碎块质心……近似为点集质心）。
///
/// 点集质心不等于体积质心，但差的那点距离只影响渲染网格与碰撞形状
/// 之间的对齐——两者用的是同一套点，所以没有可见错位。
#[derive(Debug, Clone, PartialEq)]
pub struct FracturePieceGeometry {
    /// 顶点，子刚体局部空间。
    pub points: Vec<Vec3>,
    /// 三角形索引，每三个一组，来自 parry 的凸包。
    pub indices: Vec<u32>,
}

/// 一次成功碎裂的产物。
#[derive(Debug, Clone, PartialEq)]
pub struct FracturePieces {
    /// 两个子刚体。
    pub bodies: [BodyHandle; 2],
    /// 两个子碰撞体，与 `bodies` 按下标对应。
    pub colliders: [ColliderHandle; 2],
    /// 两个子刚体的渲染几何，与 `bodies` 按下标对应。
    /// 场景侧拿它建碎块的显示网格。
    pub geometry: [FracturePieceGeometry; 2],
}

/// 点进平面哪一侧。`eps` 以内的算贴在平面上，两边都要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Positive,
    Negative,
    OnPlane,
}

fn classify(dist: f32, eps: f32) -> Side {
    if dist > eps {
        Side::Positive
    } else if dist < -eps {
        Side::Negative
    } else {
        Side::OnPlane
    }
}

/// 立方体的 8 个角点（局部空间），下标按位表示 (±x, ±y, ±z)。
fn cuboid_corners(half_extents: Vec3) -> [Vec3; 8] {
    let h = half_extents;
    [
        Vec3::new(-h.x, -h.y, -h.z),
        Vec3::new(h.x, -h.y, -h.z),
        Vec3::new(-h.x, h.y, -h.z),
        Vec3::new(h.x, h.y, -h.z),
        Vec3::new(-h.x, -h.y, h.z),
        Vec3::new(h.x, -h.y, h.z),
        Vec3::new(-h.x, h.y, h.z),
        Vec3::new(h.x, h.y, h.z),
    ]
}

/// 立方体的 12 条棱，用角点下标表示。同一条棱的两个端点恰好差一个坐标位。
fn cuboid_edges() -> [(usize, usize); 12] {
    let mut edges = [(0, 0); 12];
    let mut n = 0;
    for i in 0..8 {
        for axis in 0..3 {
            let j = i ^ (1 << axis);
            if j > i {
                edges[n] = (i, j);
                n += 1;
            }
        }
    }
    edges
}

/// 把立方体沿平面切成两份点集（世界空间）。
///
/// 跨过平面的棱与平面求交，交点两边都收——这样两侧点集各自的凸包
/// 都是闭合的，切面精确。返回 `None` 表示平面根本没切到
/// （所有角点都在同一侧）或切出来的是擦边薄片。
fn split_cuboid(
    half_extents: Vec3,
    pose: (Vec3, Quat),
    plane: &FracturePlane,
    min_thickness: f32,
) -> Option<(Vec<Vec3>, Vec<Vec3>)> {
    let (position, rotation) = pose;
    let corners: Vec<Vec3> = cuboid_corners(half_extents)
        .iter()
        .map(|c| position + rotation * *c)
        .collect();
    let dists: Vec<f32> = corners.iter().map(|c| plane.signed_distance(*c)).collect();

    // 角点坐标量级就是形状尺寸，eps 按尺寸缩放，不然小碎块永远在平面上。
    let size = half_extents.length().max(1e-6);
    let eps = size * 1e-6;

    let mut positive: Vec<Vec3> = Vec::with_capacity(12);
    let mut negative: Vec<Vec3> = Vec::with_capacity(12);
    for (corner, dist) in corners.iter().zip(dists.iter()) {
        match classify(*dist, eps) {
            Side::Positive | Side::OnPlane => positive.push(*corner),
            _ => {}
        }
        match classify(*dist, eps) {
            Side::Negative | Side::OnPlane => negative.push(*corner),
            _ => {}
        }
    }

    // 跨平面的棱贡献切割面顶点。
    for (a, b) in cuboid_edges() {
        let (da, db) = (dists[a], dists[b]);
        let sa = classify(da, eps);
        let sb = classify(db, eps);
        if (sa == Side::Positive && sb == Side::Negative)
            || (sa == Side::Negative && sb == Side::Positive)
        {
            let t = da / (da - db);
            let p = corners[a] + (corners[b] - corners[a]) * t;
            positive.push(p);
            negative.push(p);
        }
    }

    reject_slivers(&positive, &negative, plane, min_thickness)
}

/// 把凸包点集沿平面切成两份（世界空间），近似切法，见模块文档。
fn split_point_cloud(
    points: &[Vec3],
    plane: &FracturePlane,
    min_thickness: f32,
) -> Option<(Vec<Vec3>, Vec<Vec3>)> {
    if points.is_empty() {
        return None;
    }
    let size = points
        .iter()
        .map(|p| (*p - plane.point).length())
        .fold(0.0f32, f32::max)
        .max(1e-6);
    let eps = size * 1e-6;

    let mut positive = Vec::new();
    let mut negative = Vec::new();
    for p in points {
        match classify(plane.signed_distance(*p), eps) {
            Side::Positive | Side::OnPlane => positive.push(*p),
            _ => {}
        }
        match classify(plane.signed_distance(*p), eps) {
            Side::Negative | Side::OnPlane => negative.push(*p),
            _ => {}
        }
    }

    reject_slivers(&positive, &negative, plane, min_thickness)
}

/// 擦边检查：某一侧为空（平面没切到），或某一侧沿法线太薄（擦个皮）。
/// 两种都算切失败，返回 `None`。
fn reject_slivers(
    positive: &[Vec3],
    negative: &[Vec3],
    plane: &FracturePlane,
    min_thickness: f32,
) -> Option<(Vec<Vec3>, Vec<Vec3>)> {
    if positive.len() < 4 || negative.len() < 4 {
        return None;
    }
    // 两侧都贴着平面（交点进了点集），厚度看各自离平面最远的点。
    let thickness = |side: &[Vec3]| {
        side.iter()
            .map(|p| plane.signed_distance(*p).abs())
            .fold(0.0f32, f32::max)
    };
    if thickness(positive) < min_thickness || thickness(negative) < min_thickness {
        return None;
    }
    Some((positive.to_vec(), negative.to_vec()))
}

/// 点集质心。只用来定子刚体的位置，不参与质量计算——
/// 质量由碰撞形状的体积 × 密度得出，引擎自己会算。
fn centroid(points: &[Vec3]) -> Vec3 {
    let sum = points.iter().fold(Vec3::ZERO, |acc, p| acc + *p);
    sum / points.len() as f32
}

/// 对点集做凸包，失败（点共面 / 共线 / 太少）时返回 `None`。
/// 返回的索引拍平成 `u32` 列表，场景侧建渲染网格直接能用。
fn convex_hull_of(points: &[Vec3]) -> Option<(Vec<Vec3>, Vec<u32>)> {
    let rv: Vec<_> = points.iter().copied().map(to_rv).collect();
    let (verts, idx) = rapier3d::parry::transformation::try_convex_hull(&rv).ok()?;
    if verts.len() < 4 || idx.is_empty() {
        return None;
    }
    Some((
        verts.into_iter().map(from_rv).collect(),
        idx.into_iter().flat_map(|t| [t[0], t[1], t[2]]).collect(),
    ))
}

impl crate::PhysicsWorld {
    /// 读两个碰撞体之间最深接触点的世界坐标。
    ///
    /// 碎裂平面需要一个「切在哪里」的点，接触力事件只给方向不给位置，
    /// 这里补上。没有接触（或接触已分开）时返回 `None`，
    /// 调用方应回落到母体质心。
    pub fn contact_point(
        &self,
        collider_a: ColliderHandle,
        collider_b: ColliderHandle,
    ) -> Option<Vec3> {
        let pair = self
            .inner
            .narrow_phase
            .contact_pair(collider_a.0, collider_b.0)?;
        let (_, contact) = pair.find_deepest_contact()?;
        // 注意 `contact_pair(a, b)` 不保证返回的 pair 里 `collider1 == a`：
        // rapier 内部按自己的顺序存，调用方传参顺序不影响（见上测试的 a_is_1=false）。
        // 所以必须按「哪边是 collider1」选局部点，不能假设 local_p1 属于 a。
        // 两边局部点各自用自家位姿变到世界后取中点：中点就是接触面位置，
        // 与调用方传参顺序无关，深穿透时也是重叠区的中部，合理。
        let (local_a, local_b) = if pair.collider1 == collider_a.0 {
            (contact.local_p1, contact.local_p2)
        } else {
            (contact.local_p2, contact.local_p1)
        };
        let pose_a = self.inner.colliders.get(collider_a.0)?.position();
        let pose_b = self.inner.colliders.get(collider_b.0)?.position();
        let wa = from_rv(pose_a.transform_point(local_a));
        let wb = from_rv(pose_b.transform_point(local_b));
        Some((wa + wb) * 0.5)
    }

    /// 用世界空间平面把一个碰撞体（及其所属刚体）切成两块。
    ///
    /// 只支持单碰撞体刚体：母体删掉，两个子刚体继承母体的位姿朝向、
    /// 线/角速度、阻尼、重力倍率、支配组、CCD、休眠与启用开关，
    /// 碰撞体继承摩擦、弹性、合成规则、分组、密度、传感器、碰撞事件
    /// 与启用开关。接触力阈值默认继承母体（负值继承、非负显式指定，
    /// 0 表示静默——碎块限代靠它）。
    ///
    /// 切不动时返回 `None` 且世界保持原样：
    /// 形状不支持（非立方体/非凸包）、平面没切到、切出薄片、
    /// 凸包重建失败、多碰撞体刚体、句柄无效。
    pub fn fracture_collider(
        &mut self,
        collider: ColliderHandle,
        plane: &FracturePlane,
        opts: &FractureOptions,
    ) -> Option<FracturePieces> {
        let native_collider = self.inner.colliders.get(collider.0)?;
        let body_handle = native_collider.parent()?;
        let native_body = self.inner.bodies.get(body_handle)?;

        // 多碰撞体刚体不碰：其他碰撞体挂到哪块上都会改变质量分布，
        // 不如明确拒绝，让调用方先拆。
        if native_body.colliders().len() != 1 {
            return None;
        }

        // 世界位姿：碰撞体的世界位姿已经包含了刚体位姿 × 相对偏移，
        // 母体形状的顶点统一搬到世界空间再切。
        let (col_pos, col_rot) = crate::convert::from_rp(native_collider.position());
        let world_points: Vec<Vec3> = match native_collider.shape().as_cuboid() {
            Some(cuboid) => {
                let h = from_rv(cuboid.half_extents);
                // 退化盒子（零厚度）切出来全是薄片，直接拒掉。
                if h.x <= 0.0 || h.y <= 0.0 || h.z <= 0.0 {
                    return None;
                }
                let (pos, neg) = split_cuboid(h, (col_pos, col_rot), plane, opts.min_thickness)?;
                // 立方体走精确分支：两侧点集各自直接就是结果。
                return self.spawn_pieces(body_handle, collider, [pos, neg], plane, opts);
            }
            None => match native_collider.shape().as_convex_polyhedron() {
                Some(convex) => convex
                    .points()
                    .iter()
                    .map(|p| col_pos + col_rot * from_rv(*p))
                    .collect(),
                None => return None,
            },
        };

        let (pos, neg) = split_point_cloud(&world_points, plane, opts.min_thickness)?;
        self.spawn_pieces(body_handle, collider, [pos, neg], plane, opts)
    }

    /// 按两份世界空间点集生成子刚体。
    ///
    /// 两侧点集都要过一次凸包重建，统一走这里是让
    /// 「先算好再删母体」的顺序只写一遍。
    #[allow(clippy::too_many_arguments)]
    fn spawn_pieces(
        &mut self,
        body_handle: rapier3d::dynamics::RigidBodyHandle,
        collider_handle: ColliderHandle,
        sides: [Vec<Vec3>; 2],
        plane: &FracturePlane,
        opts: &FractureOptions,
    ) -> Option<FracturePieces> {
        let native_body = self.inner.bodies.get(body_handle)?;
        let native_collider = self.inner.colliders.get(collider_handle.0)?;

        // 母体的运动状态与材质先读出来。刚体类型不继承：
        // 静态的东西被切了还静止就太假了，碎块一律动态。
        let body_type = crate::body::RigidBodyType::Dynamic;
        let (_, body_rot) = crate::convert::from_rp(native_body.position());
        let linvel = from_rv(native_body.linvel());
        let angvel = from_rv(native_body.angvel());
        let linear_damping = native_body.linear_damping();
        let angular_damping = native_body.angular_damping();
        let gravity_scale = native_body.gravity_scale();
        let dominance_group = native_body.dominance_group();
        let ccd_enabled = native_body.is_ccd_enabled();
        let locked = native_body.locked_axes();
        let body_user_data = native_body.user_data;
        // 休眠开关没有直接 getter：`can_sleep(false)` 会把激活阈值设成负数，
        // 看阈值符号反推。启用开关有 getter。
        let can_sleep = native_body.activation().normalized_linear_threshold >= 0.0
            && native_body.activation().angular_threshold >= 0.0;
        let body_enabled = native_body.is_enabled();

        let friction = native_collider.friction();
        let restitution = native_collider.restitution();
        let friction_rule =
            CoefficientCombineRule::from_rapier(native_collider.friction_combine_rule());
        let restitution_rule =
            CoefficientCombineRule::from_rapier(native_collider.restitution_combine_rule());
        let collision_groups = InteractionGroups::from_rapier(native_collider.collision_groups());
        let solver_groups = InteractionGroups::from_rapier(native_collider.solver_groups());
        let density = native_collider.density();
        let is_sensor = native_collider.is_sensor();
        let collider_user_data = native_collider.user_data;
        // 事件开关直接继承：碎块再碎、再报碰撞都要不断连。
        // 接触力阈值 `<= 0` 继承母体，给正值才覆盖。
        let emit_collision_events = native_collider
            .active_events()
            .contains(rapier3d::pipeline::ActiveEvents::COLLISION_EVENTS);
        let collider_enabled = native_collider.is_enabled();
        let contact_threshold = if opts.child_contact_threshold >= 0.0 {
            opts.child_contact_threshold
        } else {
            native_collider.contact_force_event_threshold()
        };

        // 两块全算好：质心、局部点集、凸包、刚体描述、碰撞体描述。
        // 任何一块失败都直接返回，母体还没动过，世界原样。
        let mut built = Vec::with_capacity(2);
        for (side_index, points) in sides.iter().enumerate() {
            let center = centroid(points);
            // 子刚体朝向沿用母体，点集转到子刚体局部空间。
            let moved: Vec<Vec3> = points
                .iter()
                .map(|p| body_rot.inverse() * (*p - center))
                .collect();
            let (hull_points, hull_indices) = convex_hull_of(&moved)?;
            let shape = ColliderShape::ConvexHull(Arc::new(hull_points.clone()));
            // 凸包重建过了，这里再验一次只是确认形状能进求解器。
            shape.to_shared_shape()?;

            let sign = if side_index == 0 { 1.0 } else { -1.0 };
            let kick = plane.normal * (sign * opts.separation_kick.max(0.0));
            let body_desc = RigidBodyDesc {
                body_type,
                position: center,
                rotation: body_rot,
                linvel: linvel + kick,
                angvel,
                linear_damping,
                angular_damping,
                gravity_scale,
                additional_mass: 0.0,
                locked_translations: [
                    locked.contains(rapier3d::dynamics::LockedAxes::TRANSLATION_LOCKED_X),
                    locked.contains(rapier3d::dynamics::LockedAxes::TRANSLATION_LOCKED_Y),
                    locked.contains(rapier3d::dynamics::LockedAxes::TRANSLATION_LOCKED_Z),
                ],
                locked_rotations: [
                    locked.contains(rapier3d::dynamics::LockedAxes::ROTATION_LOCKED_X),
                    locked.contains(rapier3d::dynamics::LockedAxes::ROTATION_LOCKED_Y),
                    locked.contains(rapier3d::dynamics::LockedAxes::ROTATION_LOCKED_Z),
                ],
                ccd_enabled,
                can_sleep,
                dominance_group,
                enabled: body_enabled,
            };
            let mut collider_desc = ColliderDesc::new(shape)
                .with_material(friction, restitution)
                .with_density(density);
            collider_desc.friction_combine_rule = friction_rule;
            collider_desc.restitution_combine_rule = restitution_rule;
            collider_desc.collision_groups = collision_groups;
            collider_desc.solver_groups = solver_groups;
            collider_desc.is_sensor = is_sensor;
            collider_desc.emit_collision_events = emit_collision_events;
            collider_desc.enabled = collider_enabled;
            if contact_threshold > 0.0 {
                collider_desc = collider_desc.with_contact_force_threshold(contact_threshold);
            }
            built.push((body_desc, collider_desc, hull_points, hull_indices));
        }

        // 全算好了，现在才动世界：删母体（`remove_body`/`add_body`
        // 自己会标查询结构过期），加两块。
        self.remove_body(crate::BodyHandle(body_handle));

        let body_ud = opts.child_user_data.unwrap_or([body_user_data; 2]);
        let col_ud = opts
            .child_collider_user_data
            .unwrap_or([collider_user_data; 2]);
        let mut bodies = Vec::with_capacity(2);
        let mut colliders = Vec::with_capacity(2);
        let mut geometry = Vec::with_capacity(2);
        for (i, (body_desc, collider_desc, hull_points, hull_indices)) in
            built.into_iter().enumerate()
        {
            let body = self.add_body(&body_desc, body_ud[i]);
            let col = self.add_collider(&collider_desc, Some(body), col_ud[i])?;
            bodies.push(body);
            colliders.push(col);
            geometry.push(FracturePieceGeometry {
                points: hull_points,
                indices: hull_indices,
            });
        }

        Some(FracturePieces {
            bodies: [bodies[0], bodies[1]],
            colliders: [colliders[0], colliders[1]],
            geometry: [geometry.remove(0), geometry.remove(0)],
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{ColliderDesc, PhysicsWorld, RigidBodyDesc};

    fn plane_through_origin(normal: Vec3) -> FracturePlane {
        FracturePlane::new(Vec3::ZERO, normal).unwrap()
    }

    #[test]
    fn cuboid_split_in_half_gives_two_closed_pieces() {
        let plane = plane_through_origin(Vec3::X);
        let (pos, neg) = split_cuboid(
            Vec3::new(1.0, 2.0, 3.0),
            (Vec3::ZERO, Quat::IDENTITY),
            &plane,
            1e-3,
        )
        .unwrap();

        // 右半 4 角 + 4 个切面点，左半同理。
        assert_eq!(pos.len(), 8);
        assert_eq!(neg.len(), 8);
        // 切面点都贴在平面上。
        for p in pos.iter().chain(neg.iter()) {
            if p.x.abs() < 1e-6 {
                assert!(plane.signed_distance(*p).abs() < 1e-4);
            }
        }
        // 两块都能建成凸包。
        assert!(convex_hull_of(&pos).is_some());
        assert!(convex_hull_of(&neg).is_some());
    }

    #[test]
    fn plane_missing_the_shape_gives_none() {
        let plane = FracturePlane::new(Vec3::new(10.0, 0.0, 0.0), Vec3::X).unwrap();
        assert!(split_cuboid(Vec3::ONE, (Vec3::ZERO, Quat::IDENTITY), &plane, 1e-3,).is_none());
    }

    #[test]
    fn grazing_cut_is_rejected_as_sliver() {
        // 平面离表面只有 1e-4，薄片侧厚度不够。
        let plane = FracturePlane::new(Vec3::new(0.9999, 0.0, 0.0), Vec3::X).unwrap();
        assert!(split_cuboid(Vec3::ONE, (Vec3::ZERO, Quat::IDENTITY), &plane, 1e-3,).is_none());
    }

    #[test]
    fn degenerate_normal_gives_no_plane() {
        assert!(FracturePlane::new(Vec3::ZERO, Vec3::ZERO).is_none());
        assert!(FracturePlane::new(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)).is_some());
    }

    #[test]
    fn off_center_cut_still_gives_two_pieces() {
        // 平面偏左 0.5：左侧是薄块但厚度 0.5，通过；右侧正常。
        let plane = FracturePlane::new(Vec3::new(-0.5, 0.0, 0.0), Vec3::X).unwrap();
        let (pos, neg) =
            split_cuboid(Vec3::ONE, (Vec3::ZERO, Quat::IDENTITY), &plane, 1e-3).unwrap();
        assert!(convex_hull_of(&pos).is_some());
        assert!(convex_hull_of(&neg).is_some());
    }

    #[test]
    fn fracture_world_roundtrip_spawns_two_dynamic_pieces() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(
            &RigidBodyDesc::dynamic()
                .with_position(Vec3::new(0.0, 5.0, 0.0))
                .with_linvel(Vec3::new(1.0, 0.0, 0.0)),
            7,
        );
        let collider = world
            .add_collider(&ColliderDesc::cuboid(Vec3::splat(0.5)), Some(body), 9)
            .unwrap();

        let plane = plane_through_origin(Vec3::X);
        let pieces = world
            .fracture_collider(collider, &plane, &FractureOptions::default())
            .unwrap();

        // 母体没了，世界里多出两块。
        assert!(!world.has_body(body));
        assert!(!world.has_collider(collider));
        assert_eq!(world.body_count(), 2);
        assert_eq!(world.collider_count(), 2);

        // 两块都是动态的，速度 = 母体速度 ± 分离速度。
        for (i, b) in pieces.bodies.iter().enumerate() {
            let view = world.body(*b).unwrap();
            assert_eq!(view.body_type(), crate::RigidBodyType::Dynamic);
            let sign = if i == 0 { 1.0 } else { -1.0 };
            let expected = Vec3::new(1.0 + sign * 0.5, 0.0, 0.0);
            assert!(
                (view.linvel() - expected).length() < 1e-5,
                "{:?} vs {expected:?}",
                view.linvel()
            );
            // 用户数据默认继承。
            assert_eq!(view.user_data(), 7);
            assert_eq!(world.collider(pieces.colliders[i]).unwrap().user_data(), 9);
        }

        // 两块质心在平面两侧。
        let p0 = world.body(pieces.bodies[0]).unwrap().position();
        let p1 = world.body(pieces.bodies[1]).unwrap().position();
        assert!(p0.x > 0.0 && p1.x < 0.0);

        // 渲染几何齐了，索引数是 3 的倍数。
        for g in &pieces.geometry {
            assert!(!g.indices.is_empty());
            assert_eq!(g.indices.len() % 3, 0);
        }

        // 跑 60 帧，两块都往下掉且没炸。
        for _ in 0..60 {
            world.step(1.0 / 60.0);
        }
        for b in pieces.bodies {
            let y = world.body(b).unwrap().position().y;
            assert!(y < 5.0 && y > -50.0, "y = {y}");
        }
    }

    #[test]
    fn fracture_inherits_events_threshold_and_sleep_flags() {
        let mut world = PhysicsWorld::new();
        let mut body_desc = RigidBodyDesc::dynamic().with_can_sleep(false);
        body_desc.enabled = false;
        let body = world.add_body(&body_desc, 3);
        let mut col_desc = ColliderDesc::cuboid(Vec3::splat(0.5))
            .with_collision_events()
            .with_contact_force_threshold(42.0);
        col_desc.enabled = false;
        let collider = world.add_collider(&col_desc, Some(body), 5).unwrap();

        // 默认选项：阈值继承母体的 42。
        let plane = plane_through_origin(Vec3::X);
        let pieces = world
            .fracture_collider(collider, &plane, &FractureOptions::default())
            .unwrap();
        for i in 0..2 {
            let b = world.body(pieces.bodies[i]).unwrap();
            assert!(!b.can_sleep());
            assert!(!b.is_enabled());
            let c = world.collider(pieces.colliders[i]).unwrap();
            assert!(c.emits_collision_events());
            assert!(!c.is_enabled());
            assert_eq!(c.contact_force_threshold(), 42.0);
        }

        // 显式阈值覆盖继承。
        let body2 = world.add_body(&RigidBodyDesc::dynamic(), 3);
        let col2 = world
            .add_collider(&ColliderDesc::cuboid(Vec3::splat(0.5)), Some(body2), 5)
            .unwrap();
        let opts = FractureOptions {
            child_contact_threshold: 7.0,
            ..FractureOptions::default()
        };
        let pieces2 = world.fracture_collider(col2, &plane, &opts).unwrap();
        for c in pieces2.colliders {
            assert_eq!(world.collider(c).unwrap().contact_force_threshold(), 7.0);
        }

        // 显式 0 表示静默：即使母体带阈值，碎块也不上报——限代靠它。
        let body3 = world.add_body(&RigidBodyDesc::dynamic(), 3);
        let col3 = world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::splat(0.5)).with_contact_force_threshold(42.0),
                Some(body3),
                5,
            )
            .unwrap();
        let silent = FractureOptions {
            child_contact_threshold: 0.0,
            ..FractureOptions::default()
        };
        let pieces3 = world.fracture_collider(col3, &plane, &silent).unwrap();
        for c in pieces3.colliders {
            assert_eq!(world.collider(c).unwrap().contact_force_threshold(), 0.0);
        }
    }

    #[test]
    fn fracture_unsupported_shape_leaves_world_untouched() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(&RigidBodyDesc::dynamic(), 1);
        let collider = world
            .add_collider(&ColliderDesc::ball(0.5), Some(body), 1)
            .unwrap();

        let plane = plane_through_origin(Vec3::Y);
        assert!(
            world
                .fracture_collider(collider, &plane, &FractureOptions::default())
                .is_none()
        );
        assert!(world.has_body(body));
        assert!(world.has_collider(collider));
    }

    #[test]
    fn fracture_multi_collider_body_is_rejected() {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(&RigidBodyDesc::dynamic(), 1);
        let c1 = world
            .add_collider(&ColliderDesc::cuboid(Vec3::splat(0.5)), Some(body), 1)
            .unwrap();
        let _c2 = world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::splat(0.25)).with_offset(Vec3::Y, Quat::IDENTITY),
                Some(body),
                2,
            )
            .unwrap();

        let plane = plane_through_origin(Vec3::X);
        assert!(
            world
                .fracture_collider(c1, &plane, &FractureOptions::default())
                .is_none()
        );
        assert!(world.has_body(body));
    }

    #[test]
    fn contact_point_reports_world_position_between_resting_bodies() {
        let mut world = PhysicsWorld::new();
        let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
        let ground_col = world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)),
                Some(ground),
                0,
            )
            .unwrap();
        let ball = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(0.0, 2.0, 0.0)),
            1,
        );
        let ball_col = world
            .add_collider(&ColliderDesc::ball(0.5), Some(ball), 1)
            .unwrap();

        // 落到地上压实。
        for _ in 0..120 {
            world.step(1.0 / 60.0);
        }

        let p = world.contact_point(ball_col, ground_col).unwrap();
        // 接触点应该在球底与地面顶面之间（y ≈ 0.5，地面顶面 y = 0.5）。
        assert!(p.y > 0.3 && p.y < 0.7, "contact y = {} (point {p:?})", p.y);
        assert!(p.x.abs() < 1.0 && p.z.abs() < 1.0);
        // 传参顺序反过来结果应该一致（实现里按 pair 内部顺序选边）。
        let q = world.contact_point(ground_col, ball_col).unwrap();
        assert!(
            (p - q).length() < 1e-4,
            "传参顺序不该影响结果：{p:?} vs {q:?}"
        );
    }
}
