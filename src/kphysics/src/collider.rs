//! 碰撞体：形状、材质参数、交互分组。

use crate::convert::{from_rq, from_rv, to_rp, to_rv};
use kmath::{Quat, Vec3};
use kmesh::Mesh;
use rapier3d::geometry as rg;
use std::sync::Arc;

/// 三角网格碰撞体的几何数据。
///
/// 用 [`Arc`] 共享：同一张地形网格被十个碰撞体引用时，顶点只存一份。
/// 三角网格的构建（BVH）是所有形状里最贵的，重复构建的代价很实在。
#[derive(Debug, Clone, PartialEq)]
pub struct TriMeshData {
    /// 顶点，局部空间。
    pub vertices: Vec<Vec3>,
    /// 三角形索引，每三个一组。
    pub indices: Vec<[u32; 3]>,
}

impl TriMeshData {
    /// 从顶点与索引列表构造。索引数不是 3 的倍数时，多出来的会被丢弃。
    pub fn new(vertices: Vec<Vec3>, indices: &[u32]) -> Self {
        Self {
            vertices,
            indices: indices
                .chunks_exact(3)
                .map(|c| [c[0], c[1], c[2]])
                .collect(),
        }
    }

    /// 从渲染用的网格提取几何。
    ///
    /// 只取位置，法线 / UV / 切线一概不要——碰撞检测用不上，
    /// 而 `kmesh::Vertex` 一个就有几十字节。
    pub fn from_mesh(mesh: &Mesh) -> Self {
        Self::new(
            mesh.vertices().iter().map(|v| v.position()).collect(),
            mesh.indices(),
        )
    }

    /// 三角形数量。
    pub fn triangle_count(&self) -> usize {
        self.indices.len()
    }
}

/// 胶囊 / 圆柱 / 圆锥的中轴方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Axis {
    /// 沿 X 轴。
    X,
    /// 沿 Y 轴。竖直的角色胶囊用这个。
    #[default]
    Y,
    /// 沿 Z 轴。
    Z,
}

/// 碰撞体的几何形状。
///
/// 凸形状（球 / 盒 / 胶囊 / 圆柱 / 圆锥 / 凸包）之间的碰撞走通用凸体算法，
/// 又快又稳；三角网格是**非凸且无体积**的，只能当静态几何用——
/// 两个三角网格之间不会产生接触，动态的三角网格也算不出合理的质量分布。
/// 会动的复杂物体应该用凸包，或者拆成几个凸形状的复合体。
#[derive(Debug, Clone, PartialEq)]
pub enum ColliderShape {
    /// 球。
    Ball {
        /// 半径。
        radius: f32,
    },
    /// 轴对齐长方体。
    Cuboid {
        /// 三个方向上的**半长**，不是全长。
        half_extents: Vec3,
    },
    /// 胶囊：一段线段加一个半径。角色碰撞体的标准选择——没有棱角，
    /// 不会卡在台阶缝里。
    Capsule {
        /// 中轴的半长（不含两端的半球）。
        half_height: f32,
        /// 半径。
        radius: f32,
        /// 中轴方向。
        axis: Axis,
    },
    /// 圆柱。
    Cylinder {
        /// 半高。
        half_height: f32,
        /// 半径。
        radius: f32,
        /// 中轴方向。
        axis: Axis,
    },
    /// 圆锥，尖端朝轴的正方向。
    Cone {
        /// 半高。
        half_height: f32,
        /// 底面半径。
        radius: f32,
        /// 中轴方向。
        axis: Axis,
    },
    /// 无限大的半空间，法线一侧是「外面」。做无边界地面比大盒子省事。
    HalfSpace {
        /// 朝外的法线。
        normal: Vec3,
    },
    /// 三角网格。**只适合静态几何**（见类型说明）。
    TriMesh(Arc<TriMeshData>),
    /// 点集的凸包。构建时会自动算包络，凹进去的部分会被填平。
    ConvexHull(Arc<Vec<Vec3>>),
    /// 高度场。`heights` 按行主序排 `rows × cols` 个高度值。
    Heightfield {
        /// 行数（沿 Z）。
        rows: usize,
        /// 列数（沿 X）。
        cols: usize,
        /// 行主序的高度值。
        heights: Arc<Vec<f32>>,
        /// 整体缩放：X/Z 是覆盖范围，Y 是高度倍率。
        scale: Vec3,
    },
    /// 复合形状：一组带局部位姿的子形状。
    ///
    /// 这是给会动的复杂物体用的正解——把凹形状拆成几块凸的。
    Compound(Vec<(Vec3, Quat, ColliderShape)>),
}

impl ColliderShape {
    /// 球。
    pub fn ball(radius: f32) -> Self {
        Self::Ball { radius }
    }

    /// 长方体，参数是**半长**。
    pub fn cuboid(half_extents: Vec3) -> Self {
        Self::Cuboid { half_extents }
    }

    /// 沿 Y 轴的胶囊。
    pub fn capsule_y(half_height: f32, radius: f32) -> Self {
        Self::Capsule {
            half_height,
            radius,
            axis: Axis::Y,
        }
    }

    /// 三角网格，取自渲染网格。
    pub fn trimesh_from_mesh(mesh: &Mesh) -> Self {
        Self::TriMesh(Arc::new(TriMeshData::from_mesh(mesh)))
    }

    /// 凸包，取自渲染网格的顶点。
    pub fn convex_hull_from_mesh(mesh: &Mesh) -> Self {
        Self::ConvexHull(Arc::new(
            mesh.vertices().iter().map(|v| v.position()).collect(),
        ))
    }

    /// 该形状能否作为动态刚体的碰撞体。
    ///
    /// 三角网格与半空间不行：前者没有内部体积、算不出质量分布，
    /// 后者是无限大的。复合形状的结论由子形状递归决定。
    pub fn supports_dynamic_body(&self) -> bool {
        match self {
            ColliderShape::TriMesh(_)
            | ColliderShape::HalfSpace { .. }
            | ColliderShape::Heightfield { .. } => false,
            ColliderShape::Compound(parts) => {
                parts.iter().all(|(_, _, s)| s.supports_dynamic_body())
            }
            _ => true,
        }
    }

    /// 转成 rapier 的形状。
    ///
    /// 返回 `None` 的情形都是几何本身退化了：凸包算不出来（点共面或点太少）、
    /// 三角网格构建失败、高度场尺寸对不上。这些只能在构建时才发现，
    /// 所以签名是 `Option` 而不是无脑 unwrap——一个坏模型不该让整个游戏 panic。
    pub(crate) fn to_shared_shape(&self) -> Option<rg::SharedShape> {
        Some(match self {
            ColliderShape::Ball { radius } => rg::SharedShape::ball(*radius),
            ColliderShape::Cuboid { half_extents } => {
                rg::SharedShape::cuboid(half_extents.x, half_extents.y, half_extents.z)
            }
            ColliderShape::Capsule {
                half_height,
                radius,
                axis,
            } => match axis {
                Axis::X => rg::SharedShape::capsule_x(*half_height, *radius),
                Axis::Y => rg::SharedShape::capsule_y(*half_height, *radius),
                Axis::Z => rg::SharedShape::capsule_z(*half_height, *radius),
            },
            // rapier 的圆柱与圆锥只有沿 Y 的版本，其他轴向靠一层旋转的复合形状顶上。
            ColliderShape::Cylinder {
                half_height,
                radius,
                axis,
            } => Self::oriented(rg::SharedShape::cylinder(*half_height, *radius), *axis),
            ColliderShape::Cone {
                half_height,
                radius,
                axis,
            } => Self::oriented(rg::SharedShape::cone(*half_height, *radius), *axis),
            ColliderShape::HalfSpace { normal } => {
                let normal = normal.normalize_or_zero();
                if normal == Vec3::ZERO {
                    return None;
                }
                rg::SharedShape::halfspace(to_rv(normal))
            }
            ColliderShape::TriMesh(data) => {
                if data.vertices.is_empty() || data.indices.is_empty() {
                    return None;
                }
                rg::SharedShape::trimesh(
                    data.vertices.iter().copied().map(to_rv).collect(),
                    data.indices.clone(),
                )
                .ok()?
            }
            ColliderShape::ConvexHull(points) => {
                let points: Vec<_> = points.iter().copied().map(to_rv).collect();
                rg::SharedShape::convex_hull(&points)?
            }
            ColliderShape::Heightfield {
                rows,
                cols,
                heights,
                scale,
            } => {
                if heights.len() != rows * cols || *rows < 2 || *cols < 2 {
                    return None;
                }
                rg::SharedShape::heightfield(
                    rapier3d::parry::utils::Array2::new(*rows, *cols, heights.as_ref().clone()),
                    to_rv(*scale),
                )
            }
            ColliderShape::Compound(parts) => {
                let parts: Vec<_> = parts
                    .iter()
                    .filter_map(|(p, r, s)| Some((to_rp(*p, *r), s.to_shared_shape()?)))
                    .collect();
                if parts.is_empty() {
                    return None;
                }
                rg::SharedShape::compound(parts)
            }
        })
    }

    /// 把沿 Y 的形状转到指定轴上。Y 轴原样返回，避免白套一层复合形状。
    fn oriented(shape: rg::SharedShape, axis: Axis) -> rg::SharedShape {
        let rotation = match axis {
            Axis::Y => return shape,
            Axis::X => Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
            Axis::Z => Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        };
        rg::SharedShape::compound(vec![(to_rp(Vec3::ZERO, rotation), shape)])
    }
}

/// 摩擦 / 弹性系数在两个碰撞体之间的合成方式。
///
/// 碰撞双方各有一个系数，最终用哪个由**两者中规则序号更大的那个**说了算
/// （rapier 的规则），所以给地面设 `Min` 就能强制压低任何东西的弹性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CoefficientCombineRule {
    /// 取平均。
    #[default]
    Average,
    /// 取较小值。
    Min,
    /// 相乘。
    Multiply,
    /// 取较大值。
    Max,
}

impl CoefficientCombineRule {
    pub(crate) fn to_rapier(self) -> rapier3d::dynamics::CoefficientCombineRule {
        use rapier3d::dynamics::CoefficientCombineRule as R;
        match self {
            CoefficientCombineRule::Average => R::Average,
            CoefficientCombineRule::Min => R::Min,
            CoefficientCombineRule::Multiply => R::Multiply,
            CoefficientCombineRule::Max => R::Max,
        }
    }

    /// 同上，2D 版。rapier2d 和 rapier3d 的这个枚举是两个不同的类型，
    /// 尽管长得一模一样。
    pub(crate) fn to_rapier2d(self) -> rapier2d::dynamics::CoefficientCombineRule {
        use rapier2d::dynamics::CoefficientCombineRule as R;
        match self {
            CoefficientCombineRule::Average => R::Average,
            CoefficientCombineRule::Min => R::Min,
            CoefficientCombineRule::Multiply => R::Multiply,
            CoefficientCombineRule::Max => R::Max,
        }
    }
}

/// 碰撞过滤分组。
///
/// 判定是**双向**的：A 与 B 只有在「A 的成员位∩B 的过滤位」和
/// 「B 的成员位∩A 的过滤位」都非空时才会碰撞。单方面把对方拉黑就够了，
/// 这样「子弹不打玩家」只需要改子弹一处。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InteractionGroups {
    /// 自己属于哪些组。
    pub memberships: u32,
    /// 愿意和哪些组交互。
    pub filter: u32,
}

impl Default for InteractionGroups {
    fn default() -> Self {
        Self::ALL
    }
}

impl InteractionGroups {
    /// 属于所有组，也和所有组交互。
    pub const ALL: Self = Self {
        memberships: u32::MAX,
        filter: u32::MAX,
    };

    /// 什么都不碰。
    pub const NONE: Self = Self {
        memberships: 0,
        filter: 0,
    };

    /// 自定义分组。
    pub const fn new(memberships: u32, filter: u32) -> Self {
        Self {
            memberships,
            filter,
        }
    }

    /// 按 kphysics 的规则判断两组能否交互。与 rapier 内部的判定一致。
    pub fn test(self, other: Self) -> bool {
        (self.memberships & other.filter) != 0 && (other.memberships & self.filter) != 0
    }

    pub(crate) fn to_rapier(self) -> rg::InteractionGroups {
        rg::InteractionGroups::new(
            rg::Group::from_bits_truncate(self.memberships),
            rg::Group::from_bits_truncate(self.filter),
            Default::default(),
        )
    }

    /// 同上，2D 版。
    pub(crate) fn to_rapier2d(self) -> rapier2d::geometry::InteractionGroups {
        use rapier2d::geometry as rg2;
        rg2::InteractionGroups::new(
            rg2::Group::from_bits_truncate(self.memberships),
            rg2::Group::from_bits_truncate(self.filter),
            Default::default(),
        )
    }
}

/// 建一个碰撞体所需的全部参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderDesc {
    /// 几何形状。
    pub shape: ColliderShape,
    /// 相对所属刚体的位置偏移。没有刚体时是世界空间位置。
    pub position: Vec3,
    /// 相对所属刚体的朝向偏移。
    pub rotation: Quat,
    /// 摩擦系数。
    pub friction: f32,
    /// 弹性系数。0 = 完全不弹，1 = 理论上弹回原高度。
    pub restitution: f32,
    /// 密度。质量由「密度 × 形状体积」算出。
    pub density: f32,
    /// 是否是传感器。传感器不产生碰撞响应，只报告重叠——触发区用它。
    pub is_sensor: bool,
    /// 碰撞分组。
    pub collision_groups: InteractionGroups,
    /// 求解分组。默认与碰撞分组一致；分开设可以做出
    /// 「能检测到接触但不产生推力」的效果。
    pub solver_groups: InteractionGroups,
    /// 摩擦系数的合成规则。
    pub friction_combine_rule: CoefficientCombineRule,
    /// 弹性系数的合成规则。
    pub restitution_combine_rule: CoefficientCombineRule,
    /// 是否上报碰撞开始 / 结束事件。默认关闭——事件是有成本的，
    /// 只给真正要监听的碰撞体打开。
    pub emit_collision_events: bool,
    /// 上报接触力事件的阈值（牛顿）。0 表示不上报。
    ///
    /// 碰撞事件只说「碰上了」，说不出**撞得有多狠**——而「轻轻放上去」
    /// 和「砸下来」在物理上是同一件事的两个极端。玻璃碎不碎、角色掉多少血、
    /// 撞击音效多响，靠的都是这个。
    ///
    /// 阈值不是 0/1 开关而是一个力的下限：碰撞每帧都在发生（一摞箱子
    /// 静止时每个接触点都有支撑力），阈值低了会被自重刷屏。
    /// 一个 1 千克的物体静止在地上大约是 10 牛，所以想只捕捉「砸下来」
    /// 就得给到几十上百。
    pub contact_force_threshold: f32,
    /// 是否启用。
    pub enabled: bool,
}

impl Default for ColliderDesc {
    fn default() -> Self {
        Self {
            shape: ColliderShape::ball(0.5),
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            friction: 0.5,
            restitution: 0.0,
            density: 1.0,
            is_sensor: false,
            collision_groups: InteractionGroups::ALL,
            solver_groups: InteractionGroups::ALL,
            friction_combine_rule: CoefficientCombineRule::Average,
            restitution_combine_rule: CoefficientCombineRule::Average,
            emit_collision_events: false,
            contact_force_threshold: 0.0,
            enabled: true,
        }
    }
}

impl ColliderDesc {
    /// 指定形状，其余取默认值。
    pub fn new(shape: ColliderShape) -> Self {
        Self {
            shape,
            ..Self::default()
        }
    }

    /// 球形碰撞体。
    pub fn ball(radius: f32) -> Self {
        Self::new(ColliderShape::ball(radius))
    }

    /// 盒形碰撞体，参数是半长。
    pub fn cuboid(half_extents: Vec3) -> Self {
        Self::new(ColliderShape::cuboid(half_extents))
    }

    /// 沿 Y 的胶囊碰撞体。
    pub fn capsule_y(half_height: f32, radius: f32) -> Self {
        Self::new(ColliderShape::capsule_y(half_height, radius))
    }

    /// 指定相对刚体的偏移。
    pub fn with_offset(mut self, position: Vec3, rotation: Quat) -> Self {
        self.position = position;
        self.rotation = rotation;
        self
    }

    /// 指定摩擦与弹性。
    pub fn with_material(mut self, friction: f32, restitution: f32) -> Self {
        self.friction = friction;
        self.restitution = restitution;
        self
    }

    /// 指定密度。
    pub fn with_density(mut self, density: f32) -> Self {
        self.density = density;
        self
    }

    /// 设为传感器。
    pub fn as_sensor(mut self) -> Self {
        self.is_sensor = true;
        self
    }

    /// 指定碰撞分组（求解分组一并设成同一个）。
    pub fn with_groups(mut self, groups: InteractionGroups) -> Self {
        self.collision_groups = groups;
        self.solver_groups = groups;
        self
    }

    /// 开启碰撞事件上报。
    pub fn with_collision_events(mut self) -> Self {
        self.emit_collision_events = true;
        self
    }

    /// 开启接触力上报，力超过 `threshold` 牛才报。
    ///
    /// 给 0 或负数等于关掉。
    pub fn with_contact_force_threshold(mut self, threshold: f32) -> Self {
        self.contact_force_threshold = threshold.max(0.0);
        self
    }

    pub(crate) fn build(&self, user_data: u128) -> Option<rg::Collider> {
        let shape = self.shape.to_shared_shape()?;

        let mut events = rapier3d::pipeline::ActiveEvents::empty();
        if self.emit_collision_events {
            events |= rapier3d::pipeline::ActiveEvents::COLLISION_EVENTS;
        }
        // 接触力事件是另一个开关。只开碰撞事件的话力事件一条都不会来，
        // 而那正是「撞得够狠才碎」要的东西。
        if self.contact_force_threshold > 0.0 {
            events |= rapier3d::pipeline::ActiveEvents::CONTACT_FORCE_EVENTS;
        }

        Some(
            rg::ColliderBuilder::new(shape)
                .position(to_rp(self.position, self.rotation))
                .friction(self.friction)
                .restitution(self.restitution)
                .density(self.density)
                .sensor(self.is_sensor)
                .collision_groups(self.collision_groups.to_rapier())
                .solver_groups(self.solver_groups.to_rapier())
                .friction_combine_rule(self.friction_combine_rule.to_rapier())
                .restitution_combine_rule(self.restitution_combine_rule.to_rapier())
                .active_events(events)
                .contact_force_event_threshold(self.contact_force_threshold)
                .enabled(self.enabled)
                .user_data(user_data)
                .build(),
        )
    }
}

/// 原生碰撞体的只读视图。
pub struct ColliderRef<'a>(pub(crate) &'a rg::Collider);

impl ColliderRef<'_> {
    /// 世界空间位置。
    pub fn position(&self) -> Vec3 {
        from_rv(self.0.position().translation)
    }

    /// 世界空间朝向。
    pub fn rotation(&self) -> Quat {
        from_rq(self.0.rotation())
    }

    /// 是否是传感器。
    pub fn is_sensor(&self) -> bool {
        self.0.is_sensor()
    }

    /// 摩擦系数。
    pub fn friction(&self) -> f32 {
        self.0.friction()
    }

    /// 弹性系数。
    pub fn restitution(&self) -> f32 {
        self.0.restitution()
    }

    /// 按当前密度算出的质量。
    pub fn mass(&self) -> f32 {
        self.0.mass()
    }

    /// 用户数据。
    pub fn user_data(&self) -> u128 {
        self.0.user_data
    }
}

/// 原生碰撞体的可变视图。
pub struct ColliderMut<'a>(pub(crate) &'a mut rg::Collider);

impl ColliderMut<'_> {
    /// 降级成只读视图。
    pub fn as_ref(&self) -> ColliderRef<'_> {
        ColliderRef(self.0)
    }

    /// 设置摩擦系数。
    pub fn set_friction(&mut self, friction: f32) {
        self.0.set_friction(friction);
    }

    /// 设置弹性系数。
    pub fn set_restitution(&mut self, restitution: f32) {
        self.0.set_restitution(restitution);
    }

    /// 切换传感器状态。
    pub fn set_sensor(&mut self, is_sensor: bool) {
        self.0.set_sensor(is_sensor);
    }

    /// 启用 / 禁用。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.0.set_enabled(enabled);
    }

    /// 设置碰撞分组。
    pub fn set_collision_groups(&mut self, groups: InteractionGroups) {
        self.0.set_collision_groups(groups.to_rapier());
    }

    /// 设置相对刚体的偏移。没有刚体时相当于设世界位姿。
    pub fn set_offset(&mut self, position: Vec3, rotation: Quat) {
        self.0.set_position_wrt_parent(to_rp(position, rotation));
    }

    /// 直接设置世界位姿。只对没有所属刚体的碰撞体有意义。
    pub fn set_position(&mut self, position: Vec3, rotation: Quat) {
        self.0.set_position(to_rp(position, rotation));
    }

    /// 换一个形状。返回 `false` 表示形状退化、构建失败，原形状保持不变。
    pub fn set_shape(&mut self, shape: &ColliderShape) -> bool {
        match shape.to_shared_shape() {
            Some(s) => {
                self.0.set_shape(s);
                true
            }
            None => false,
        }
    }

    /// 设置密度，会连带刷新所属刚体的质量。
    pub fn set_density(&mut self, density: f32) {
        self.0.set_density(density);
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
    fn every_primitive_shape_builds() {
        let shapes = [
            ColliderShape::ball(1.0),
            ColliderShape::cuboid(Vec3::splat(0.5)),
            ColliderShape::capsule_y(1.0, 0.3),
            ColliderShape::Cylinder {
                half_height: 1.0,
                radius: 0.5,
                axis: Axis::X,
            },
            ColliderShape::Cone {
                half_height: 1.0,
                radius: 0.5,
                axis: Axis::Z,
            },
            ColliderShape::HalfSpace { normal: Vec3::Y },
        ];

        for shape in shapes {
            assert!(shape.to_shared_shape().is_some(), "{shape:?} 构建失败");
        }
    }

    #[test]
    fn degenerate_geometry_returns_none_instead_of_panicking() {
        // 一个坏模型不该让整个游戏挂掉——退化几何必须走 None 这条路。
        assert!(
            ColliderShape::HalfSpace { normal: Vec3::ZERO }
                .to_shared_shape()
                .is_none()
        );
        assert!(
            ColliderShape::TriMesh(Arc::new(TriMeshData {
                vertices: vec![],
                indices: vec![],
            }))
            .to_shared_shape()
            .is_none()
        );
        // 三个共线的点撑不出凸包。
        assert!(
            ColliderShape::ConvexHull(Arc::new(vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0]))
                .to_shared_shape()
                .is_none()
        );
        assert!(ColliderShape::Compound(vec![]).to_shared_shape().is_none());
    }

    #[test]
    fn heightfield_rejects_mismatched_dimensions() {
        let bad = ColliderShape::Heightfield {
            rows: 3,
            cols: 3,
            heights: Arc::new(vec![0.0; 8]),
            scale: Vec3::ONE,
        };
        assert!(bad.to_shared_shape().is_none());

        let good = ColliderShape::Heightfield {
            rows: 3,
            cols: 3,
            heights: Arc::new(vec![0.0; 9]),
            scale: Vec3::ONE,
        };
        assert!(good.to_shared_shape().is_some());
    }

    #[test]
    fn only_volumetric_shapes_support_dynamic_bodies() {
        assert!(ColliderShape::ball(1.0).supports_dynamic_body());
        assert!(!ColliderShape::HalfSpace { normal: Vec3::Y }.supports_dynamic_body());
        assert!(
            !ColliderShape::TriMesh(Arc::new(TriMeshData::new(vec![], &[])))
                .supports_dynamic_body()
        );

        // 复合形状里混进一个三角网格，整体就不能当动态刚体用了。
        let mixed = ColliderShape::Compound(vec![
            (Vec3::ZERO, Quat::IDENTITY, ColliderShape::ball(1.0)),
            (
                Vec3::ZERO,
                Quat::IDENTITY,
                ColliderShape::TriMesh(Arc::new(TriMeshData::new(vec![], &[]))),
            ),
        ]);
        assert!(!mixed.supports_dynamic_body());
    }

    #[test]
    fn non_y_axis_capsules_use_dedicated_shapes_not_compounds() {
        // 胶囊三个轴向 rapier 都有原生实现，不该退化成复合形状。
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let shape = ColliderShape::Capsule {
                half_height: 1.0,
                radius: 0.25,
                axis,
            }
            .to_shared_shape()
            .unwrap();
            assert!(shape.as_capsule().is_some(), "{axis:?} 胶囊退化了");
        }
    }

    #[test]
    fn y_axis_cylinder_is_not_wrapped_in_a_compound() {
        // 只有 X/Z 需要套旋转；Y 白套一层会平白多一次形状间接寻址。
        let y = ColliderShape::Cylinder {
            half_height: 1.0,
            radius: 0.5,
            axis: Axis::Y,
        }
        .to_shared_shape()
        .unwrap();
        assert!(y.as_cylinder().is_some());

        let x = ColliderShape::Cylinder {
            half_height: 1.0,
            radius: 0.5,
            axis: Axis::X,
        }
        .to_shared_shape()
        .unwrap();
        assert!(x.as_compound().is_some());
    }

    #[test]
    fn interaction_groups_need_agreement_from_both_sides() {
        let bullet = InteractionGroups::new(0b01, 0b10);
        let wall = InteractionGroups::new(0b10, 0b11);
        let player = InteractionGroups::new(0b100, 0b111);

        assert!(bullet.test(wall));
        assert!(wall.test(bullet));
        // 玩家愿意和子弹碰，但子弹的过滤位里没有玩家 —— 单方面拉黑就够了。
        assert!(!bullet.test(player));
        assert!(!player.test(bullet));
    }

    #[test]
    fn trimesh_data_drops_the_trailing_partial_triangle() {
        let data = TriMeshData::new(vec![Vec3::ZERO; 4], &[0, 1, 2, 3]);
        assert_eq!(data.triangle_count(), 1);
    }

    #[test]
    fn trimesh_from_mesh_keeps_triangle_count() {
        let mesh = Mesh::cube();
        let data = TriMeshData::from_mesh(&mesh);

        assert_eq!(data.triangle_count(), mesh.triangle_count());
        assert_eq!(data.vertices.len(), mesh.vertices().len());
    }
}
