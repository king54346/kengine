//! 物理描述结构的序列化。
//!
//! 存的是**描述**，不是模拟状态。刚体的类型、质量、碰撞体的形状与材质参数
//! 都会存下来；当前的位置与速度不存——位置由场景节点提供，速度重新开始。
//! 这正是「读档从一个静止的初始状态开始」的语义，也免去了在文件里
//! 复刻求解器内部状态的麻烦。
//!
//! 枚举的类型标签一律**显式写死**。靠声明顺序的话，将来在中间插一个变体，
//! 老文件里的球会被读成胶囊，而且不报错，只是形状莫名其妙。

use crate::{
    Axis, CoefficientCombineRule, ColliderDesc, ColliderShape, InteractionGroups, JointDesc,
    JointKind, RigidBodyDesc, RigidBodyType, SphericalLimits, TriMeshData,
};
use kcore::visitor::{BinaryBlob, Visit, VisitResult, Visitor, error::VisitError};
use kmath::{Quat, Vec3};
use std::sync::Arc;

fn unknown(kind: &str, tag: u8) -> VisitError {
    VisitError::User(format!("未知的{kind}类型标签 {tag}"))
}

impl Visit for RigidBodyType {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut tag = match self {
            Self::Dynamic => 0u8,
            Self::Fixed => 1,
            Self::KinematicPositionBased => 2,
            Self::KinematicVelocityBased => 3,
        };
        tag.visit(name, visitor)?;

        if visitor.is_reading() {
            *self = match tag {
                0 => Self::Dynamic,
                1 => Self::Fixed,
                2 => Self::KinematicPositionBased,
                3 => Self::KinematicVelocityBased,
                other => return Err(unknown("刚体", other)),
            };
        }
        Ok(())
    }
}

impl Visit for Axis {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut tag = match self {
            Self::X => 0u8,
            Self::Y => 1,
            Self::Z => 2,
        };
        tag.visit(name, visitor)?;

        if visitor.is_reading() {
            *self = match tag {
                0 => Self::X,
                1 => Self::Y,
                2 => Self::Z,
                other => return Err(unknown("轴向", other)),
            };
        }
        Ok(())
    }
}

impl Visit for CoefficientCombineRule {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut tag = match self {
            Self::Average => 0u8,
            Self::Min => 1,
            Self::Multiply => 2,
            Self::Max => 3,
        };
        tag.visit(name, visitor)?;

        if visitor.is_reading() {
            *self = match tag {
                0 => Self::Average,
                1 => Self::Min,
                2 => Self::Multiply,
                3 => Self::Max,
                other => return Err(unknown("合成规则", other)),
            };
        }
        Ok(())
    }
}

impl Visit for InteractionGroups {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.memberships.visit("Memberships", &mut region)?;
        self.filter.visit("Filter", &mut region)?;
        Ok(())
    }
}

impl Visit for TriMeshData {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        // 走二进制大块：一张地形碰撞网格可能有几十万个三角形。
        // glam 的 `Vec3` 没开 bytemuck 特性，先摊成 `[f32; 3]`。
        let mut vertices: Vec<[f32; 3]> = self.vertices.iter().map(|v| v.to_array()).collect();
        BinaryBlob {
            vec: &mut vertices,
        }
        .visit("Vertices", &mut region)?;
        BinaryBlob {
            vec: &mut self.indices,
        }
        .visit("Indices", &mut region)?;

        if region.is_reading() {
            self.vertices = vertices.into_iter().map(Vec3::from_array).collect();
        }

        Ok(())
    }
}

impl ColliderShape {
    fn tag(&self) -> u8 {
        match self {
            Self::Ball { .. } => 0,
            Self::Cuboid { .. } => 1,
            Self::Capsule { .. } => 2,
            Self::Cylinder { .. } => 3,
            Self::Cone { .. } => 4,
            Self::HalfSpace { .. } => 5,
            Self::TriMesh(_) => 6,
            Self::ConvexHull(_) => 7,
            Self::Heightfield { .. } => 8,
            Self::Compound(_) => 9,
        }
    }
}

impl Visit for ColliderShape {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut tag = self.tag();
        tag.visit("Tag", &mut region)?;

        if region.is_reading() {
            // 先按标签造一个同类型的空壳，下面的分支两个方向就能共用一套代码。
            *self = match tag {
                0 => Self::Ball { radius: 0.0 },
                1 => Self::Cuboid {
                    half_extents: Vec3::ZERO,
                },
                2 => Self::Capsule {
                    half_height: 0.0,
                    radius: 0.0,
                    axis: Axis::Y,
                },
                3 => Self::Cylinder {
                    half_height: 0.0,
                    radius: 0.0,
                    axis: Axis::Y,
                },
                4 => Self::Cone {
                    half_height: 0.0,
                    radius: 0.0,
                    axis: Axis::Y,
                },
                5 => Self::HalfSpace { normal: Vec3::Y },
                6 => Self::TriMesh(Arc::new(TriMeshData {
                    vertices: Vec::new(),
                    indices: Vec::new(),
                })),
                7 => Self::ConvexHull(Arc::new(Vec::new())),
                8 => Self::Heightfield {
                    rows: 0,
                    cols: 0,
                    heights: Arc::new(Vec::new()),
                    scale: Vec3::ONE,
                },
                9 => Self::Compound(Vec::new()),
                other => return Err(unknown("碰撞体形状", other)),
            };
        }

        match self {
            Self::Ball { radius } => radius.visit("Radius", &mut region)?,
            Self::Cuboid { half_extents } => half_extents.visit("HalfExtents", &mut region)?,
            Self::Capsule {
                half_height,
                radius,
                axis,
            }
            | Self::Cylinder {
                half_height,
                radius,
                axis,
            }
            | Self::Cone {
                half_height,
                radius,
                axis,
            } => {
                half_height.visit("HalfHeight", &mut region)?;
                radius.visit("Radius", &mut region)?;
                axis.visit("Axis", &mut region)?;
            }
            Self::HalfSpace { normal } => normal.visit("Normal", &mut region)?,
            Self::TriMesh(data) => Arc::make_mut(data).visit("Data", &mut region)?,
            Self::ConvexHull(points) => {
                let points = Arc::make_mut(points);
                let mut raw: Vec<[f32; 3]> = points.iter().map(|p| p.to_array()).collect();
                BinaryBlob { vec: &mut raw }.visit("Points", &mut region)?;
                if region.is_reading() {
                    *points = raw.into_iter().map(Vec3::from_array).collect();
                }
            }
            Self::Heightfield {
                rows,
                cols,
                heights,
                scale,
            } => {
                rows.visit("Rows", &mut region)?;
                cols.visit("Cols", &mut region)?;
                BinaryBlob {
                    vec: Arc::make_mut(heights),
                }
                .visit("Heights", &mut region)?;
                scale.visit("Scale", &mut region)?;
            }
            Self::Compound(parts) => {
                let mut count = parts.len() as u32;
                count.visit("Count", &mut region)?;
                if region.is_reading() {
                    *parts = vec![
                        (Vec3::ZERO, Quat::IDENTITY, ColliderShape::ball(0.0));
                        count as usize
                    ];
                }
                for (index, (position, rotation, shape)) in parts.iter_mut().enumerate() {
                    let mut part = region.enter_region(&format!("Part{index}"))?;
                    position.visit("Position", &mut part)?;
                    rotation.visit("Rotation", &mut part)?;
                    shape.visit("Shape", &mut part)?;
                }
            }
        }

        Ok(())
    }
}

impl Visit for ColliderDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.shape.visit("Shape", &mut region)?;
        self.position.visit("Position", &mut region)?;
        self.rotation.visit("Rotation", &mut region)?;
        self.friction.visit("Friction", &mut region)?;
        self.restitution.visit("Restitution", &mut region)?;
        self.density.visit("Density", &mut region)?;
        self.is_sensor.visit("IsSensor", &mut region)?;
        self.collision_groups.visit("CollisionGroups", &mut region)?;
        self.solver_groups.visit("SolverGroups", &mut region)?;
        self.friction_combine_rule
            .visit("FrictionCombine", &mut region)?;
        self.restitution_combine_rule
            .visit("RestitutionCombine", &mut region)?;
        self.emit_collision_events
            .visit("EmitCollisionEvents", &mut region)?;
        self.enabled.visit("Enabled", &mut region)?;

        Ok(())
    }
}

impl Visit for RigidBodyDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.body_type.visit("BodyType", &mut region)?;
        // 位置与朝向不存：刚体的位姿由所在的场景节点决定，
        // 存下来只会多一份可能和节点对不上的数据。
        self.linear_damping.visit("LinearDamping", &mut region)?;
        self.angular_damping.visit("AngularDamping", &mut region)?;
        self.gravity_scale.visit("GravityScale", &mut region)?;
        self.additional_mass.visit("AdditionalMass", &mut region)?;
        self.locked_translations
            .visit("LockedTranslations", &mut region)?;
        self.locked_rotations.visit("LockedRotations", &mut region)?;
        self.ccd_enabled.visit("Ccd", &mut region)?;
        self.can_sleep.visit("CanSleep", &mut region)?;
        self.dominance_group.visit("Dominance", &mut region)?;
        self.enabled.visit("Enabled", &mut region)?;

        Ok(())
    }
}

impl Visit for SphericalLimits {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        visit_limit("X", &mut self.x, &mut region)?;
        visit_limit("Y", &mut self.y, &mut region)?;
        visit_limit("Z", &mut self.z, &mut region)?;
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
            Self::Spherical { .. } => 1,
            Self::Revolute { .. } => 2,
            Self::Prismatic { .. } => 3,
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
                1 => Self::Spherical {
                    limits: SphericalLimits::default(),
                },
                2 => Self::Revolute {
                    axis: Vec3::X,
                    limits: None,
                },
                3 => Self::Prismatic {
                    axis: Vec3::X,
                    limits: None,
                },
                other => return Err(unknown("关节", other)),
            };
        }

        match self {
            Self::Fixed => {}
            Self::Spherical { limits } => limits.visit("Limits", &mut region)?,
            Self::Revolute { axis, limits } | Self::Prismatic { axis, limits } => {
                axis.visit("Axis", &mut region)?;
                visit_limit("Limits", limits, &mut region)?;
            }
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
        self.local_basis1.visit("Basis1", &mut region)?;
        self.local_basis2.visit("Basis2", &mut region)?;
        self.contacts_enabled.visit("ContactsEnabled", &mut region)?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn roundtrip<T: Visit + Default>(value: &T) -> T
    where
        T: Clone,
    {
        let mut visitor = Visitor::new();
        let mut source = value.clone();
        source.visit("V", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = T::default();
        restored.visit("V", &mut visitor).unwrap();
        restored
    }

    #[test]
    fn a_rigid_body_desc_survives_a_roundtrip() {
        let desc = RigidBodyDesc::kinematic_position_based()
            .with_damping(0.3, 0.7)
            .with_gravity_scale(0.25)
            .with_additional_mass(12.0)
            .with_locked_rotations()
            .with_ccd(true)
            .with_can_sleep(false);

        let restored = roundtrip(&desc);

        assert_eq!(restored.body_type, RigidBodyType::KinematicPositionBased);
        assert_eq!(restored.linear_damping, 0.3);
        assert_eq!(restored.angular_damping, 0.7);
        assert_eq!(restored.gravity_scale, 0.25);
        assert_eq!(restored.additional_mass, 12.0);
        assert_eq!(restored.locked_rotations, [true; 3]);
        assert!(restored.ccd_enabled);
        assert!(!restored.can_sleep);
    }

    #[test]
    fn body_position_is_deliberately_not_stored() {
        // 刚体的位姿由所在的场景节点说了算，存两份只会对不上。
        let desc = RigidBodyDesc::dynamic().with_position(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(roundtrip(&desc).position, Vec3::ZERO);
    }

    #[test]
    fn every_primitive_shape_survives_a_roundtrip() {
        let shapes = [
            ColliderShape::ball(1.5),
            ColliderShape::cuboid(Vec3::new(1.0, 2.0, 3.0)),
            ColliderShape::Capsule {
                half_height: 0.7,
                radius: 0.3,
                axis: Axis::X,
            },
            ColliderShape::Cylinder {
                half_height: 0.7,
                radius: 0.3,
                axis: Axis::Z,
            },
            ColliderShape::Cone {
                half_height: 0.7,
                radius: 0.3,
                axis: Axis::Y,
            },
            ColliderShape::HalfSpace { normal: Vec3::Y },
        ];

        for shape in shapes {
            let mut visitor = Visitor::new();
            let mut source = shape.clone();
            source.visit("S", &mut visitor).unwrap();
            let bytes = visitor.save_binary_to_vec().unwrap();

            let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
            let mut restored = ColliderShape::ball(0.0);
            restored.visit("S", &mut visitor).unwrap();

            assert_eq!(restored, shape, "{shape:?} 往返后变了");
        }
    }

    #[test]
    fn a_trimesh_shape_survives_a_roundtrip() {
        let shape = ColliderShape::TriMesh(Arc::new(TriMeshData::new(
            vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z],
            &[0, 1, 2, 0, 2, 3],
        )));

        let mut visitor = Visitor::new();
        let mut source = shape.clone();
        source.visit("S", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = ColliderShape::ball(0.0);
        restored.visit("S", &mut visitor).unwrap();

        assert_eq!(restored, shape);
        // 往返之后还得能真的建出形状来。
        assert!(restored.to_shared_shape().is_some());
    }

    #[test]
    fn a_compound_shape_survives_a_roundtrip() {
        let shape = ColliderShape::Compound(vec![
            (Vec3::X, Quat::from_rotation_y(0.5), ColliderShape::ball(0.4)),
            (
                Vec3::NEG_X,
                Quat::IDENTITY,
                ColliderShape::cuboid(Vec3::splat(0.2)),
            ),
        ]);

        let mut visitor = Visitor::new();
        let mut source = shape.clone();
        source.visit("S", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = ColliderShape::ball(0.0);
        restored.visit("S", &mut visitor).unwrap();

        let ColliderShape::Compound(parts) = &restored else {
            panic!("类型都变了：{restored:?}");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].2, ColliderShape::ball(0.4));
        assert!((parts[0].1 * Vec3::X - Quat::from_rotation_y(0.5) * Vec3::X).length() < 1e-5);
    }

    #[test]
    fn a_heightfield_survives_a_roundtrip() {
        let shape = ColliderShape::Heightfield {
            rows: 3,
            cols: 3,
            heights: Arc::new(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
            scale: Vec3::new(10.0, 2.0, 10.0),
        };

        let mut visitor = Visitor::new();
        let mut source = shape.clone();
        source.visit("S", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = ColliderShape::ball(0.0);
        restored.visit("S", &mut visitor).unwrap();

        assert_eq!(restored, shape);
        assert!(restored.to_shared_shape().is_some());
    }

    #[test]
    fn a_collider_desc_survives_a_roundtrip() {
        let desc = ColliderDesc::capsule_y(0.6, 0.25)
            .with_material(0.9, 0.4)
            .with_density(3.0)
            .as_sensor()
            .with_groups(InteractionGroups::new(0b101, 0b011))
            .with_collision_events();

        let restored = roundtrip(&desc);

        assert_eq!(restored, desc);
    }

    #[test]
    fn joint_kinds_survive_a_roundtrip() {
        let kinds = [
            JointDesc::fixed(Vec3::X, Vec3::Y),
            JointDesc::spherical(Vec3::X, Vec3::Y, SphericalLimits::symmetric(0.6)),
            JointDesc::revolute(Vec3::X, Vec3::Y, Vec3::Z, Some([-1.0, 1.0])),
            JointDesc::prismatic(Vec3::X, Vec3::Y, Vec3::Z, None),
        ];

        for desc in kinds {
            assert_eq!(roundtrip(&desc), desc, "{desc:?} 往返后变了");
        }
    }

    #[test]
    fn optional_joint_limits_keep_their_presence() {
        // `None` 和 `Some([0, 0])` 是两回事：前者不限位，后者锁死。
        let unlimited = JointDesc::revolute(Vec3::ZERO, Vec3::ZERO, Vec3::Y, None);
        let locked = JointDesc::revolute(Vec3::ZERO, Vec3::ZERO, Vec3::Y, Some([0.0, 0.0]));

        assert_eq!(roundtrip(&unlimited), unlimited);
        assert_eq!(roundtrip(&locked), locked);
        assert_ne!(roundtrip(&unlimited), roundtrip(&locked));
    }

    #[test]
    fn an_unknown_shape_tag_is_rejected() {
        let mut visitor = Visitor::new();
        {
            let mut region = visitor.enter_region("S").unwrap();
            let mut tag = 200u8;
            tag.visit("Tag", &mut region).unwrap();
        }
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = ColliderShape::ball(0.0);

        assert!(restored.visit("S", &mut visitor).is_err());
    }
}
