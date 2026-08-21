//! 2D 碰撞体：形状、描述与借用包装。

use super::convert::{from_rv, to_rp, to_rv};
use crate::{CoefficientCombineRule, InteractionGroups};
use kmath::Vec2;
use rapier2d::geometry as rg;

/// 2D 碰撞体的形状。
#[derive(Debug, Clone, PartialEq)]
pub enum ColliderShape {
    /// 圆。
    Ball {
        /// 半径。
        radius: f32,
    },
    /// 轴对齐矩形。
    Cuboid {
        /// 两个方向上的**半长**，不是全长。
        half_extents: Vec2,
    },
    /// 圆角矩形。角上是圆弧，撞上去不会有硬棱。
    RoundCuboid {
        /// 半长（不含圆角）。
        half_extents: Vec2,
        /// 圆角半径。
        border_radius: f32,
    },
    /// 胶囊：一段线段加一个半径。
    Capsule {
        /// 中轴的半长（不含两端的半圆）。
        half_height: f32,
        /// 半径。
        radius: f32,
        /// 中轴是否沿 X 轴；否则沿 Y 轴。
        horizontal: bool,
    },
    /// 三角形。
    Triangle {
        /// 三个顶点。
        points: [Vec2; 3],
    },
    /// 线段。
    Segment {
        /// 两个端点。
        a: Vec2,
        /// 两个端点。
        b: Vec2,
    },
    /// 折线：一串首尾相连的线段。**没有内外之分**，是一条无限薄的墙。
    ///
    /// 2D 关卡的地形轮廓用它。注意折线不封闭也没关系——它本来就只是
    /// 一串边，不构成实体。想要实体请用 [`ConvexPolygon`](Self::ConvexPolygon)
    /// 或若干个 [`Cuboid`](Self::Cuboid)。
    Polyline {
        /// 顶点，按顺序连成线段。
        points: Vec<Vec2>,
    },
    /// 凸多边形。传进去的点会被求一次凸包。
    ///
    /// **非凸的形状不能直接用**——物理引擎的碰撞算法（GJK/EPA）建立在
    /// 凸性上。凹形要拆成若干个凸块，各挂一个碰撞体到同一个刚体上。
    ConvexPolygon {
        /// 点集，不必已经是凸包也不必有序。
        points: Vec<Vec2>,
    },
    /// 无限大的半平面，法线一侧是「外面」。做无边界地面比大矩形省事。
    HalfSpace {
        /// 朝外的法线。
        normal: Vec2,
    },
}

impl ColliderShape {
    fn build(&self) -> Option<rg::SharedShape> {
        let shape = match self {
            ColliderShape::Ball { radius } => rg::SharedShape::ball(*radius),
            ColliderShape::Cuboid { half_extents } => {
                rg::SharedShape::cuboid(half_extents.x, half_extents.y)
            }
            ColliderShape::RoundCuboid {
                half_extents,
                border_radius,
            } => rg::SharedShape::round_cuboid(half_extents.x, half_extents.y, *border_radius),
            ColliderShape::Capsule {
                half_height,
                radius,
                horizontal,
            } => {
                if *horizontal {
                    rg::SharedShape::capsule_x(*half_height, *radius)
                } else {
                    rg::SharedShape::capsule_y(*half_height, *radius)
                }
            }
            ColliderShape::Triangle { points } => {
                rg::SharedShape::triangle(to_rv(points[0]), to_rv(points[1]), to_rv(points[2]))
            }
            ColliderShape::Segment { a, b } => rg::SharedShape::segment(to_rv(*a), to_rv(*b)),
            ColliderShape::Polyline { points } => {
                // 少于两个点连不成一条线段。
                if points.len() < 2 {
                    return None;
                }
                rg::SharedShape::polyline(points.iter().map(|p| to_rv(*p)).collect(), None)
            }
            ColliderShape::ConvexPolygon { points } => {
                // 凸包可能失败：所有点共线时构不成多边形。
                // 返回 None 而不是塞一个退化形状进去——退化形状会让
                // 求解器算出 NaN，然后整个世界飞出去。
                rg::SharedShape::convex_hull(
                    &points.iter().map(|p| to_rv(*p)).collect::<Vec<_>>(),
                )?
            }
            ColliderShape::HalfSpace { normal } => {
                let normalized = normal.normalize_or_zero();
                if normalized == Vec2::ZERO {
                    return None;
                }
                rg::SharedShape::halfspace(to_rv(normalized))
            }
        };
        Some(shape)
    }
}

/// 建一个 2D 碰撞体所需的全部参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderDesc {
    /// 形状。
    pub shape: ColliderShape,
    /// 相对所属刚体的位置。没有刚体时就是世界位置。
    pub position: Vec2,
    /// 相对所属刚体的朝向，弧度。
    pub rotation: f32,
    /// 密度。质量由它乘以形状面积算出来。
    pub density: f32,
    /// 摩擦系数。
    pub friction: f32,
    /// 弹性系数。1 表示完全弹回，0 表示不弹。
    pub restitution: f32,
    /// 摩擦系数怎么和对方的合成。
    pub friction_combine_rule: CoefficientCombineRule,
    /// 弹性系数怎么和对方的合成。
    pub restitution_combine_rule: CoefficientCombineRule,
    /// 是否是传感器。传感器只报告重叠，不产生碰撞响应——触发区用它。
    pub sensor: bool,
    /// 碰撞过滤组。
    pub collision_groups: InteractionGroups,
    /// 求解组：过滤「产生接触力」，但仍然会报告碰撞事件。
    pub solver_groups: InteractionGroups,
    /// 是否上报碰撞开始 / 结束事件。默认关闭——事件是有成本的，
    /// 只给真正要监听的碰撞体打开。
    ///
    /// **传感器不受这个开关限制**：传感器不开事件的话什么都不做，
    /// 既不碰撞也不报告，没有任何用途。所以传感器一律自动开启。
    pub emit_collision_events: bool,
    /// 是否报告接触力事件。
    pub contact_force_events: bool,
    /// 触发接触力事件的力阈值。
    pub contact_force_threshold: f32,
    /// 是否启用。
    pub enabled: bool,
}

impl Default for ColliderDesc {
    fn default() -> Self {
        Self {
            shape: ColliderShape::Ball { radius: 0.5 },
            position: Vec2::ZERO,
            rotation: 0.0,
            density: 1.0,
            friction: 0.5,
            restitution: 0.0,
            friction_combine_rule: CoefficientCombineRule::Average,
            restitution_combine_rule: CoefficientCombineRule::Average,
            sensor: false,
            emit_collision_events: false,
            collision_groups: InteractionGroups::default(),
            solver_groups: InteractionGroups::default(),
            contact_force_events: false,
            contact_force_threshold: 0.0,
            enabled: true,
        }
    }
}

impl ColliderDesc {
    /// 一个圆。
    pub fn ball(radius: f32) -> Self {
        Self {
            shape: ColliderShape::Ball { radius },
            ..Default::default()
        }
    }

    /// 一个矩形。`half_extents` 是**半长**。
    pub fn cuboid(half_extents: Vec2) -> Self {
        Self {
            shape: ColliderShape::Cuboid { half_extents },
            ..Default::default()
        }
    }

    /// 一个竖着的胶囊。
    pub fn capsule(half_height: f32, radius: f32) -> Self {
        Self {
            shape: ColliderShape::Capsule {
                half_height,
                radius,
                horizontal: false,
            },
            ..Default::default()
        }
    }

    /// 一条折线，2D 关卡的地形轮廓用它。
    pub fn polyline(points: Vec<Vec2>) -> Self {
        Self {
            shape: ColliderShape::Polyline { points },
            ..Default::default()
        }
    }

    /// 一个凸多边形。
    pub fn convex_polygon(points: Vec<Vec2>) -> Self {
        Self {
            shape: ColliderShape::ConvexPolygon { points },
            ..Default::default()
        }
    }

    /// 一个半平面。
    pub fn half_space(normal: Vec2) -> Self {
        Self {
            shape: ColliderShape::HalfSpace { normal },
            ..Default::default()
        }
    }

    /// 设置相对位置。
    pub fn with_position(mut self, position: Vec2) -> Self {
        self.position = position;
        self
    }

    /// 设置密度。
    pub fn with_density(mut self, density: f32) -> Self {
        self.density = density;
        self
    }

    /// 设置摩擦。
    pub fn with_friction(mut self, friction: f32) -> Self {
        self.friction = friction;
        self
    }

    /// 设置弹性。
    pub fn with_restitution(mut self, restitution: f32) -> Self {
        self.restitution = restitution;
        self
    }

    /// 变成传感器。碰撞事件会自动开启。
    pub fn as_sensor(mut self) -> Self {
        self.sensor = true;
        self
    }

    /// 开启碰撞事件上报。
    pub fn with_collision_events(mut self) -> Self {
        self.emit_collision_events = true;
        self
    }

    /// 设置碰撞过滤组。
    pub fn with_collision_groups(mut self, groups: InteractionGroups) -> Self {
        self.collision_groups = groups;
        self
    }

    /// 形状能不能建出来。
    ///
    /// 退化的形状（共线的凸包、只有一个点的折线、零法线的半平面）
    /// 建不出来——把它们塞给求解器会算出 NaN，然后整个世界飞出去。
    pub fn is_valid(&self) -> bool {
        self.shape.build().is_some()
    }

    pub(crate) fn build(&self, user_data: u128) -> Option<rg::Collider> {
        let shape = self.shape.build()?;
        let mut events = rapier2d::pipeline::ActiveEvents::empty();
        // 传感器一律开启：不开的话它什么都不做。
        if self.emit_collision_events || self.sensor {
            events |= rapier2d::pipeline::ActiveEvents::COLLISION_EVENTS;
        }
        if self.contact_force_events {
            events |= rapier2d::pipeline::ActiveEvents::CONTACT_FORCE_EVENTS;
        }

        let mut builder = rg::ColliderBuilder::new(shape)
            .active_events(events)
            .position(to_rp(self.position, self.rotation))
            .density(self.density)
            .friction(self.friction)
            .restitution(self.restitution)
            .friction_combine_rule(self.friction_combine_rule.to_rapier2d())
            .restitution_combine_rule(self.restitution_combine_rule.to_rapier2d())
            .sensor(self.sensor)
            .collision_groups(self.collision_groups.to_rapier2d())
            .solver_groups(self.solver_groups.to_rapier2d())
            .enabled(self.enabled)
            .user_data(user_data);

        if self.contact_force_events {
            builder = builder.contact_force_event_threshold(self.contact_force_threshold);
        }
        // 传感器默认不报告和静态物体的重叠。触发区多半就是贴在
        // 静态几何上的，不开这个的话它永远不触发。
        builder = builder.active_collision_types(rg::ActiveCollisionTypes::all());
        Some(builder.build())
    }
}

/// 只读地看一个 2D 碰撞体。
pub struct ColliderRef<'a> {
    pub(crate) inner: &'a rg::Collider,
}

impl ColliderRef<'_> {
    /// 世界空间位置。
    pub fn position(&self) -> Vec2 {
        from_rv(self.inner.position().translation)
    }

    /// 是不是传感器。
    pub fn is_sensor(&self) -> bool {
        self.inner.is_sensor()
    }

    /// 摩擦系数。
    pub fn friction(&self) -> f32 {
        self.inner.friction()
    }

    /// 弹性系数。
    pub fn restitution(&self) -> f32 {
        self.inner.restitution()
    }

    /// 密度。
    pub fn density(&self) -> f32 {
        self.inner.density()
    }

    /// 是否启用。
    pub fn is_enabled(&self) -> bool {
        self.inner.is_enabled()
    }

    /// 建它时塞进去的用户数据。
    pub fn user_data(&self) -> u128 {
        self.inner.user_data
    }
}

/// 可写地操作一个 2D 碰撞体。
pub struct ColliderMut<'a> {
    pub(crate) inner: &'a mut rg::Collider,
}

impl ColliderMut<'_> {
    /// 设置相对刚体的位置与朝向。
    pub fn set_position(&mut self, position: Vec2, rotation: f32) {
        self.inner.set_position_wrt_parent(to_rp(position, rotation));
    }

    /// 设置摩擦。
    pub fn set_friction(&mut self, friction: f32) {
        self.inner.set_friction(friction);
    }

    /// 设置弹性。
    pub fn set_restitution(&mut self, restitution: f32) {
        self.inner.set_restitution(restitution);
    }

    /// 设为 / 取消传感器。
    pub fn set_sensor(&mut self, sensor: bool) {
        self.inner.set_sensor(sensor);
    }

    /// 设置碰撞过滤组。
    pub fn set_collision_groups(&mut self, groups: InteractionGroups) {
        self.inner.set_collision_groups(groups.to_rapier2d());
    }

    /// 启用 / 禁用。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.inner.set_enabled(enabled);
    }
}
