//! 2D 物理描述结构的序列化。
//!
//! 和 3D 那份（[`crate::visit`]）同一套规矩：
//!
//! - 存的是**描述**，不是模拟状态。类型、质量、形状、材质参数都存；
//!   当前位置与速度不存——位置由场景节点提供，速度重新开始。
//!   这正是「读档从一个静止的初始状态开始」的语义。
//! - 枚举的类型标签**显式写死**。靠声明顺序的话，将来在中间插一个变体，
//!   老文件里的圆会被读成胶囊，而且不报错，只是形状莫名其妙。
//!
//! 共享的那几个类型（[`RigidBodyType`](crate::RigidBodyType)、
//! [`CoefficientCombineRule`](crate::CoefficientCombineRule)、
//! [`InteractionGroups`](crate::InteractionGroups)）2D 和 3D 用的是同一个，
//! 它们的实现在 3D 那份里，这里直接复用。

use super::{ColliderDesc, ColliderShape, JointDesc, JointKind, RigidBodyDesc};
use kcore::visitor::{Visit, VisitResult, Visitor, error::VisitError};
use kmath::Vec2;

fn unknown(kind: &str, tag: u8) -> VisitError {
    VisitError::User(format!("未知的 2D {kind}类型标签 {tag}"))
}

/// 读写一个点列表。
///
/// 手工展开成「一个长度 + 若干个点」而不是靠 `Vec<Vec2>` 的 `Visit`：
/// 折线与凸多边形的点数是可变的，读的时候要先知道有几个才好分配。
fn visit_points(name: &str, points: &mut Vec<Vec2>, visitor: &mut Visitor) -> VisitResult {
    let mut region = visitor.enter_region(name)?;

    let mut count = points.len() as u32;
    count.visit("Count", &mut region)?;

    if region.is_reading() {
        points.clear();
        points.reserve(count as usize);
        for index in 0..count {
            let mut point = Vec2::ZERO;
            point.visit(&format!("P{index}"), &mut region)?;
            points.push(point);
        }
    } else {
        for (index, point) in points.iter_mut().enumerate() {
            point.visit(&format!("P{index}"), &mut region)?;
        }
    }
    Ok(())
}

impl ColliderShape {
    fn tag(&self) -> u8 {
        match self {
            Self::Ball { .. } => 0,
            Self::Cuboid { .. } => 1,
            Self::RoundCuboid { .. } => 2,
            Self::Capsule { .. } => 3,
            Self::Triangle { .. } => 4,
            Self::Segment { .. } => 5,
            Self::Polyline { .. } => 6,
            Self::ConvexPolygon { .. } => 7,
            Self::HalfSpace { .. } => 8,
        }
    }
}

impl Visit for ColliderShape {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut tag = self.tag();
        tag.visit("Tag", &mut region)?;

        // 读的时候先按标签造一个同类型的空壳，下面两个方向就能共用一套代码。
        if region.is_reading() {
            *self = match tag {
                0 => Self::Ball { radius: 0.5 },
                1 => Self::Cuboid {
                    half_extents: Vec2::splat(0.5),
                },
                2 => Self::RoundCuboid {
                    half_extents: Vec2::splat(0.5),
                    border_radius: 0.1,
                },
                3 => Self::Capsule {
                    half_height: 0.5,
                    radius: 0.25,
                    horizontal: false,
                },
                4 => Self::Triangle {
                    points: [Vec2::ZERO; 3],
                },
                5 => Self::Segment {
                    a: Vec2::ZERO,
                    b: Vec2::X,
                },
                6 => Self::Polyline { points: Vec::new() },
                7 => Self::ConvexPolygon { points: Vec::new() },
                8 => Self::HalfSpace { normal: Vec2::Y },
                other => return Err(unknown("碰撞体形状", other)),
            };
        }

        match self {
            Self::Ball { radius } => radius.visit("Radius", &mut region)?,
            Self::Cuboid { half_extents } => half_extents.visit("HalfExtents", &mut region)?,
            Self::RoundCuboid {
                half_extents,
                border_radius,
            } => {
                half_extents.visit("HalfExtents", &mut region)?;
                border_radius.visit("BorderRadius", &mut region)?;
            }
            Self::Capsule {
                half_height,
                radius,
                horizontal,
            } => {
                half_height.visit("HalfHeight", &mut region)?;
                radius.visit("Radius", &mut region)?;
                horizontal.visit("Horizontal", &mut region)?;
            }
            Self::Triangle { points } => {
                for (index, point) in points.iter_mut().enumerate() {
                    point.visit(&format!("P{index}"), &mut region)?;
                }
            }
            Self::Segment { a, b } => {
                a.visit("A", &mut region)?;
                b.visit("B", &mut region)?;
            }
            Self::Polyline { points } => visit_points("Points", points, &mut region)?,
            Self::ConvexPolygon { points } => visit_points("Points", points, &mut region)?,
            Self::HalfSpace { normal } => normal.visit("Normal", &mut region)?,
        }

        Ok(())
    }
}

impl Visit for ColliderDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.shape.visit("Shape", &mut region)?;
        // 碰撞体的位姿是**相对刚体**的，属于形状布局的一部分，所以要存；
        // 刚体自己的位姿才是由场景节点决定的那个。
        self.position.visit("Position", &mut region)?;
        self.rotation.visit("Rotation", &mut region)?;
        self.density.visit("Density", &mut region)?;
        self.friction.visit("Friction", &mut region)?;
        self.restitution.visit("Restitution", &mut region)?;
        self.friction_combine_rule
            .visit("FrictionCombine", &mut region)?;
        self.restitution_combine_rule
            .visit("RestitutionCombine", &mut region)?;
        self.sensor.visit("Sensor", &mut region)?;
        self.collision_groups
            .visit("CollisionGroups", &mut region)?;
        self.solver_groups.visit("SolverGroups", &mut region)?;
        self.emit_collision_events
            .visit("EmitCollisionEvents", &mut region)?;
        self.contact_force_events
            .visit("ContactForceEvents", &mut region)?;
        self.contact_force_threshold
            .visit("ContactForceThreshold", &mut region)?;
        self.enabled.visit("Enabled", &mut region)?;

        Ok(())
    }
}

impl Visit for RigidBodyDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.body_type.visit("BodyType", &mut region)?;
        // 位置、朝向与速度不存，理由同 3D：刚体的位姿由所在的场景节点决定，
        // 存下来只会多一份可能和节点对不上的数据。
        self.linear_damping.visit("LinearDamping", &mut region)?;
        self.angular_damping.visit("AngularDamping", &mut region)?;
        self.gravity_scale.visit("GravityScale", &mut region)?;
        self.additional_mass.visit("AdditionalMass", &mut region)?;
        self.locked_translations
            .visit("LockedTranslations", &mut region)?;
        self.locked_rotation.visit("LockedRotation", &mut region)?;
        self.ccd_enabled.visit("Ccd", &mut region)?;
        self.can_sleep.visit("CanSleep", &mut region)?;
        self.dominance_group.visit("Dominance", &mut region)?;
        self.enabled.visit("Enabled", &mut region)?;

        Ok(())
    }
}

/// 读写一个可选的 `[最小, 最大]` 限位。
fn visit_limit(name: &str, limit: &mut Option<[f32; 2]>, visitor: &mut Visitor) -> VisitResult {
    let mut region = visitor.enter_region(name)?;

    let mut present = limit.is_some();
    present.visit("Present", &mut region)?;

    if present {
        let mut value = limit.unwrap_or([0.0; 2]);
        value[0].visit("Min", &mut region)?;
        value[1].visit("Max", &mut region)?;
        if region.is_reading() {
            *limit = Some(value);
        }
    } else if region.is_reading() {
        *limit = None;
    }

    Ok(())
}

impl JointKind {
    fn tag(&self) -> u8 {
        match self {
            Self::Fixed => 0,
            Self::Revolute { .. } => 1,
            Self::Prismatic { .. } => 2,
            Self::Rope { .. } => 3,
        }
    }
}

impl Visit for JointKind {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut tag = self.tag();
        tag.visit("Tag", &mut region)?;

        if region.is_reading() {
            *self = match tag {
                0 => Self::Fixed,
                1 => Self::Revolute { limits: None },
                2 => Self::Prismatic {
                    axis: Vec2::X,
                    limits: None,
                },
                3 => Self::Rope { max_distance: 1.0 },
                other => return Err(unknown("关节", other)),
            };
        }

        match self {
            Self::Fixed => {}
            Self::Revolute { limits } => visit_limit("Limits", limits, &mut region)?,
            Self::Prismatic { axis, limits } => {
                axis.visit("Axis", &mut region)?;
                visit_limit("Limits", limits, &mut region)?;
            }
            Self::Rope { max_distance } => max_distance.visit("MaxDistance", &mut region)?,
        }

        Ok(())
    }
}

impl Visit for JointDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.kind.visit("Kind", &mut region)?;
        self.local_anchor1.visit("Anchor1", &mut region)?;
        self.local_anchor2.visit("Anchor2", &mut region)?;
        self.contacts_enabled
            .visit("ContactsEnabled", &mut region)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoefficientCombineRule, InteractionGroups, RigidBodyType};

    /// 写进内存再读回来。
    ///
    /// `blank` 是读的时候往里填的空壳——这几个描述类型都没有 `Default`
    /// （形状和关节都得先说清楚是哪一种，没有「默认形状」这回事），
    /// 所以由调用方给一个。
    fn roundtrip<T: Visit + Clone>(value: &T, mut blank: T) -> T {
        let mut writer = Visitor::new();
        value
            .clone()
            .visit("Root", &mut writer)
            .expect("写失败");
        let bytes = writer.save_binary_to_vec().expect("序列化失败");

        let mut reader = Visitor::load_from_memory(&bytes).expect("反序列化失败");
        blank.visit("Root", &mut reader).expect("读失败");
        blank
    }

    /// 读档时用的空形状。具体是哪种无所谓，标签会把它换掉。
    fn blank_shape() -> ColliderShape {
        ColliderShape::Ball { radius: 1.0 }
    }

    /// 读档时用的空关节。
    fn blank_joint() -> JointDesc {
        JointDesc::fixed(Vec2::ZERO, Vec2::ZERO)
    }

    #[test]
    fn every_shape_survives_a_roundtrip() {
        // 九个变体逐个过一遍。漏掉哪个的话，那种形状存下去读回来会变形，
        // 而且不报错——这正是标签写死、变体逐个测试的理由。
        let shapes = [
            ColliderShape::Ball { radius: 1.25 },
            ColliderShape::Cuboid {
                half_extents: Vec2::new(2.0, 3.0),
            },
            ColliderShape::RoundCuboid {
                half_extents: Vec2::new(1.0, 2.0),
                border_radius: 0.2,
            },
            ColliderShape::Capsule {
                half_height: 1.5,
                radius: 0.4,
                horizontal: true,
            },
            ColliderShape::Triangle {
                points: [Vec2::ZERO, Vec2::X, Vec2::Y],
            },
            ColliderShape::Segment {
                a: Vec2::new(-1.0, 0.0),
                b: Vec2::new(1.0, 2.0),
            },
            ColliderShape::Polyline {
                points: vec![Vec2::ZERO, Vec2::X, Vec2::new(2.0, 1.0)],
            },
            ColliderShape::ConvexPolygon {
                points: vec![Vec2::ZERO, Vec2::X, Vec2::Y, Vec2::ONE],
            },
            ColliderShape::HalfSpace { normal: Vec2::Y },
        ];

        for shape in shapes {
            assert_eq!(
                roundtrip(&shape, blank_shape()),
                shape,
                "{shape:?} 没能原样回来"
            );
        }
    }

    #[test]
    fn an_unknown_shape_tag_is_an_error_not_a_wrong_shape() {
        // 老版本读到新版本存的形状时，宁可报错也不能静默读成别的形状。
        let mut writer = Visitor::new();
        {
            let mut region = writer.enter_region("Root").expect("region");
            let mut tag = 99u8;
            tag.visit("Tag", &mut region).expect("写标签");
        }
        let bytes = writer.save_binary_to_vec().expect("序列化");

        let mut reader = Visitor::load_from_memory(&bytes).expect("反序列化");
        let mut shape = ColliderShape::Ball { radius: 1.0 };

        assert!(shape.visit("Root", &mut reader).is_err());
    }

    #[test]
    fn a_collider_keeps_its_material_and_layout() {
        let desc = ColliderDesc {
            shape: ColliderShape::Cuboid {
                half_extents: Vec2::new(1.5, 0.5),
            },
            position: Vec2::new(0.25, -1.0),
            rotation: 0.75,
            density: 2.5,
            friction: 0.8,
            restitution: 0.3,
            friction_combine_rule: CoefficientCombineRule::Max,
            restitution_combine_rule: CoefficientCombineRule::Min,
            sensor: true,
            collision_groups: InteractionGroups::new(0b10, 0b01),
            solver_groups: InteractionGroups::new(0b11, 0b11),
            emit_collision_events: true,
            contact_force_events: true,
            contact_force_threshold: 12.5,
            enabled: false,
        };
        let restored = roundtrip(&desc, ColliderDesc::default());

        assert_eq!(restored, desc);
    }

    #[test]
    fn a_body_keeps_everything_except_its_pose() {
        // 位姿由场景节点提供，所以**故意不存**。这条测试把那个约定钉住：
        // 哪天有人顺手把位置也存进去，这里会红。
        let desc = RigidBodyDesc {
            body_type: RigidBodyType::KinematicVelocityBased,
            position: Vec2::new(7.0, 8.0),
            rotation: 1.25,
            linvel: Vec2::new(3.0, 4.0),
            angvel: 2.0,
            linear_damping: 0.5,
            angular_damping: 0.25,
            gravity_scale: 0.0,
            additional_mass: 5.0,
            locked_translations: [true, false],
            locked_rotation: true,
            ccd_enabled: true,
            can_sleep: false,
            dominance_group: 7,
            enabled: false,
        };
        let restored = roundtrip(&desc, RigidBodyDesc::default());

        assert_eq!(restored.body_type, desc.body_type);
        assert_eq!(restored.locked_translations, desc.locked_translations);
        assert!(restored.locked_rotation);
        assert_eq!(restored.dominance_group, 7);
        assert_eq!(restored.additional_mass, 5.0);
        assert!(!restored.can_sleep);

        assert_eq!(restored.position, Vec2::ZERO, "位置不该被存下来");
        assert_eq!(restored.linvel, Vec2::ZERO, "速度不该被存下来");
        assert_eq!(restored.angvel, 0.0);
    }

    #[test]
    fn every_joint_kind_survives_a_roundtrip() {
        let joints = [
            JointDesc::fixed(Vec2::new(1.0, 2.0), Vec2::ZERO),
            JointDesc::revolute(Vec2::X, Vec2::Y, Some([-0.5, 1.5])),
            JointDesc::revolute(Vec2::X, Vec2::Y, None),
            JointDesc::prismatic(Vec2::ZERO, Vec2::ONE, Vec2::Y, Some([0.0, 3.0])),
            JointDesc::rope(Vec2::ZERO, Vec2::X, 4.5).with_contacts(),
        ];

        for joint in joints {
            assert_eq!(
                roundtrip(&joint, blank_joint()),
                joint,
                "{joint:?} 没能原样回来"
            );
        }
    }

    #[test]
    fn an_absent_limit_stays_absent() {
        // `None` 和 `Some([0,0])` 是两回事：前者是「能转满一圈」，
        // 后者是「焊死在零度」。存丢了的话布娃娃会突然变僵硬。
        let joint = JointDesc::revolute(Vec2::ZERO, Vec2::ZERO, None);
        let restored = roundtrip(&joint, blank_joint());

        assert_eq!(restored.kind, JointKind::Revolute { limits: None });
    }
}
