//! klight —— 光源。
//!
//! 光源作为组件挂在场景节点上，位置与朝向来自节点的世界变换：
//! **方向光与聚光灯沿节点的 -Z 轴照射**（与 glTF 约定一致）。
//!
//! 与 [`kpbr`](https://docs.rs/kpbr) 一样提供两份实现：
//! [`LIGHT_WGSL`] 给 GPU，[`attenuation`] 给 CPU——后者让衰减曲线可以被单元测试。
//!
//! ```
//! use klight::prelude::*;
//!
//! // 点光源在作用半径处衰减到 0，不会留下"永远照得到"的拖尾。
//! let light = Light::point(10.0);
//! assert_eq!(attenuation::distance(10.0, 10.0), 0.0);
//! assert!(attenuation::distance(1.0, 10.0) > 0.0);
//! ```

#![warn(missing_docs)]

pub mod attenuation;
pub mod cascade;
pub mod shadow;

use bytemuck::{Pod, Zeroable};
use kmath::{Mat4, Vec3};

/// 前向渲染一次能处理的光源上限。
///
/// 超出的光源会被丢弃——受 uniform 缓冲大小与着色器循环开销限制。
/// 要支持更多光源需要改用延迟渲染或分簇前向渲染。
pub const MAX_LIGHTS: usize = 16;

/// Cook-Torrance 光照求值的 WGSL 源码，由渲染器拼进着色器。
pub const LIGHT_WGSL: &str = include_str!("light.wgsl");

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        GpuLight, LIGHT_WGSL, Light, LightKind, MAX_LIGHTS, attenuation, shadow::ShadowSettings,
    };
}

/// 光源类型。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LightKind {
    /// 方向光：无位置、无衰减，模拟太阳。
    Directional,
    /// 点光源：向各方向均匀发光。
    Point {
        /// 作用半径，超出后强度为 0。
        range: f32,
    },
    /// 半球光：从天空和地面两个方向来的环境光。
    ///
    /// 不是一盏「有位置的灯」，而是对**环境**的粗略近似：朝上的表面
    /// 收到天空的颜色，朝下的收到地面反射的颜色，中间按法线插值。
    ///
    /// 户外场景里它比一盏方向光顶用——只有太阳的话，背光面是纯黑的，
    /// 而现实里那一面被天空和地面照亮着。
    ///
    /// **只贡献漫反射**，不产生高光也不投影。
    Hemisphere {
        /// 地面反射的颜色。天空的颜色用 [`Light::color`]。
        ground_color: Vec3,
    },
    /// 聚光灯：锥形光束。
    Spot {
        /// 作用半径。
        range: f32,
        /// 内锥半角（角度制），锥内强度为满。
        inner_angle: f32,
        /// 外锥半角（角度制），锥外强度为 0。
        outer_angle: f32,
    },
}

impl LightKind {
    /// 该类型在 GPU 侧的标签值。
    fn tag(&self) -> f32 {
        match self {
            Self::Directional => 0.0,
            Self::Point { .. } => 1.0,
            Self::Spot { .. } => 2.0,
            Self::Hemisphere { .. } => 3.0,
        }
    }

    /// 作用半径；方向光返回 0（不参与衰减）。
    pub fn range(&self) -> f32 {
        match self {
            Self::Directional | Self::Hemisphere { .. } => 0.0,
            Self::Point { range } | Self::Spot { range, .. } => *range,
        }
    }
}

/// 一盏光源。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Light {
    /// 光源类型与几何参数。
    pub kind: LightKind,
    /// 光照颜色（线性空间）。
    pub color: Vec3,
    /// 强度。PBR 下高光很依赖它。
    pub intensity: f32,
    /// 是否启用。
    pub enabled: bool,
    /// 是否投射阴影。目前只有场景里第一盏开启此项的方向光会生效。
    pub cast_shadows: bool,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            kind: LightKind::Directional,
            color: Vec3::ONE,
            intensity: 3.0,
            enabled: true,
            cast_shadows: false,
        }
    }
}

impl LightKind {
    /// 序列化用的类型标签。与 GPU 侧的 [`tag`](Self::tag) 无关——
    /// 那个是着色器的分支依据，可以随渲染实现改；这个是文件格式的一部分，
    /// 改了就读不了老场景。
    ///
    /// 显式写死而不是靠声明顺序：将来在中间插一个变体，靠顺序的话
    /// 老场景里的点光源会被读成聚光灯，而且不报错。
    fn visit_tag(&self) -> u8 {
        match self {
            Self::Directional => 0,
            Self::Point { .. } => 1,
            Self::Spot { .. } => 2,
            Self::Hemisphere { .. } => 3,
        }
    }
}

impl kcore::visitor::Visit for LightKind {
    fn visit(
        &mut self,
        name: &str,
        visitor: &mut kcore::visitor::Visitor,
    ) -> kcore::visitor::VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut tag = self.visit_tag();
        tag.visit("Tag", &mut region)?;

        if region.is_reading() {
            *self = match tag {
                0 => Self::Directional,
                1 => Self::Point { range: 0.0 },
                2 => Self::Spot {
                    range: 0.0,
                    inner_angle: 0.0,
                    outer_angle: 0.0,
                },
                3 => Self::Hemisphere {
                    ground_color: Vec3::ZERO,
                },
                other => {
                    return Err(kcore::visitor::error::VisitError::User(format!(
                        "未知的光源类型标签 {other}"
                    )));
                }
            };
        }

        match self {
            Self::Directional => {}
            Self::Point { range } => range.visit("Range", &mut region)?,
            Self::Spot {
                range,
                inner_angle,
                outer_angle,
            } => {
                range.visit("Range", &mut region)?;
                inner_angle.visit("InnerAngle", &mut region)?;
                outer_angle.visit("OuterAngle", &mut region)?;
            }
            Self::Hemisphere { ground_color } => ground_color.visit("GroundColor", &mut region)?,
        }

        Ok(())
    }
}

impl kcore::visitor::Visit for Light {
    fn visit(
        &mut self,
        name: &str,
        visitor: &mut kcore::visitor::Visitor,
    ) -> kcore::visitor::VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.kind.visit("Kind", &mut region)?;
        self.color.visit("Color", &mut region)?;
        self.intensity.visit("Intensity", &mut region)?;
        self.enabled.visit("Enabled", &mut region)?;
        self.cast_shadows.visit("CastShadows", &mut region)?;
        Ok(())
    }
}

impl Light {
    /// 方向光。照射方向为所在节点的 -Z 轴。
    pub fn directional() -> Self {
        Self::default()
    }

    /// 点光源。
    pub fn point(range: f32) -> Self {
        Self {
            kind: LightKind::Point {
                range: range.max(0.0),
            },
            intensity: 20.0,
            ..Default::default()
        }
    }

    /// 聚光灯。内外锥角为半角（角度制），内角会被钳制到不超过外角。
    pub fn spot(range: f32, inner_angle: f32, outer_angle: f32) -> Self {
        let outer = outer_angle.clamp(0.0, 89.9);
        Self {
            kind: LightKind::Spot {
                range: range.max(0.0),
                inner_angle: inner_angle.clamp(0.0, outer),
                outer_angle: outer,
            },
            intensity: 30.0,
            ..Default::default()
        }
    }

    /// 指定颜色。
    pub fn with_color(mut self, color: Vec3) -> Self {
        self.color = color;
        self
    }

    /// 指定强度。
    pub fn with_intensity(mut self, intensity: f32) -> Self {
        self.intensity = intensity;
        self
    }

    /// 让该光源投射阴影。
    pub fn with_shadows(mut self) -> Self {
        self.cast_shadows = true;
        self
    }

    /// 光线传播方向，由节点世界变换的 -Z 轴给出。
    pub fn direction(&self, world_transform: kmath::Mat4) -> Vec3 {
        (-world_transform.z_axis.truncate()).normalize_or(Vec3::NEG_Y)
    }

    /// 按节点的世界变换打包成 GPU 数据。
    ///
    /// 位置取变换的平移分量，照射方向取 -Z 轴。
    pub fn to_gpu(&self, world_transform: Mat4) -> GpuLight {
        let position = world_transform.w_axis.truncate();
        // -Z 为朝向，与 glTF / 相机的约定一致。
        let direction = (-world_transform.z_axis.truncate()).normalize_or(Vec3::NEG_Y);

        let (cos_inner, cos_outer) = match self.kind {
            LightKind::Spot {
                inner_angle,
                outer_angle,
                ..
            } => (
                inner_angle.to_radians().cos(),
                outer_angle.to_radians().cos(),
            ),
            _ => (1.0, 0.0),
        };

        // 半球光把地面色塞在 `params.xyz` 里：它没有内外锥，那两个槽位
        // 本来就是空的。多开一个 vec4 会让每盏灯都白涨 16 字节。
        let params = match self.kind {
            LightKind::Hemisphere { ground_color } => {
                [ground_color.x, ground_color.y, ground_color.z, 0.0]
            }
            _ => [cos_inner, cos_outer, 0.0, 0.0],
        };

        GpuLight {
            position: [position.x, position.y, position.z, self.kind.tag()],
            direction: [direction.x, direction.y, direction.z, self.kind.range()],
            color: [self.color.x, self.color.y, self.color.z, self.intensity],
            params,
        }
    }
}

/// 光源的 GPU 表示，与 `light.wgsl` 里的 `Light` 结构逐字段对应。
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct GpuLight {
    /// xyz = 世界坐标，w = 类型标签。
    pub position: [f32; 4],
    /// xyz = 照射方向，w = 作用半径。
    pub direction: [f32; 4],
    /// rgb = 颜色，a = 强度。
    pub color: [f32; 4],
    /// x = 内锥余弦，y = 外锥余弦。
    pub params: [f32; 4],
}

impl Default for GpuLight {
    fn default() -> Self {
        // 全零即"强度为 0 的方向光"，不会对画面产生影响，适合填充空槽位。
        Self::zeroed()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn wgsl_source_is_valid() {
        kshader::Shader::from_wgsl(LIGHT_WGSL).expect("光照着色器代码应当合法");
    }

    #[test]
    fn wgsl_exposes_expected_functions() {
        for name in [
            "light_sample_direction",
            "light_radiance",
            "light_distance_attenuation",
            "light_spot_attenuation",
        ] {
            assert!(LIGHT_WGSL.contains(name), "WGSL 缺少函数 {name}");
        }
    }

    #[test]
    fn gpu_layout_matches_wgsl() {
        // 4 个 vec4 = 64 字节；与 light.wgsl 的 Light 结构一致。
        assert_eq!(size_of::<GpuLight>(), 64);
        assert_eq!(size_of::<GpuLight>() % 16, 0);
    }

    #[test]
    fn directional_light_points_along_negative_z() {
        let light = Light::directional();
        // 单位变换下 -Z 即 (0,0,-1)。
        let gpu = light.to_gpu(Mat4::IDENTITY);

        assert_eq!(
            [gpu.direction[0], gpu.direction[1], gpu.direction[2]],
            [0.0, 0.0, -1.0]
        );
        assert_eq!(gpu.position[3], 0.0, "方向光的类型标签应为 0");
    }

    #[test]
    fn light_position_comes_from_transform() {
        let light = Light::point(5.0);
        let transform = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));

        let gpu = light.to_gpu(transform);

        assert_eq!(
            [gpu.position[0], gpu.position[1], gpu.position[2]],
            [1.0, 2.0, 3.0]
        );
        assert_eq!(gpu.position[3], 1.0, "点光源的类型标签应为 1");
        assert_eq!(gpu.direction[3], 5.0, "作用半径应写入 direction.w");
    }

    #[test]
    fn rotation_changes_light_direction() {
        let light = Light::directional();
        // 绕 X 轴转 90°，-Z 变成 +Y 或 -Y。
        let transform = Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2);

        let gpu = light.to_gpu(transform);
        let direction = Vec3::new(gpu.direction[0], gpu.direction[1], gpu.direction[2]);

        assert!((direction.length() - 1.0).abs() < 1e-5, "方向应当归一化");
        assert!(
            direction.y.abs() > 0.99,
            "转向后应指向 Y 轴，实得 {direction:?}"
        );
    }

    #[test]
    fn spot_angles_are_clamped() {
        // 内角大于外角是无意义的，构造时钳制。
        let light = Light::spot(10.0, 80.0, 30.0);

        let LightKind::Spot {
            inner_angle,
            outer_angle,
            ..
        } = light.kind
        else {
            panic!("应当是聚光灯");
        };

        assert!(inner_angle <= outer_angle);
        assert_eq!(outer_angle, 30.0);
    }

    #[test]
    fn spot_cosines_are_ordered() {
        let light = Light::spot(10.0, 20.0, 40.0);
        let gpu = light.to_gpu(Mat4::IDENTITY);

        // 角度越小余弦越大，内锥余弦必须大于外锥余弦。
        assert!(gpu.params[0] > gpu.params[1]);
    }

    #[test]
    fn negative_range_is_clamped_to_zero() {
        assert_eq!(Light::point(-5.0).kind.range(), 0.0);
    }

    #[test]
    fn shadows_are_opt_in() {
        assert!(!Light::directional().cast_shadows);
        assert!(Light::directional().with_shadows().cast_shadows);
    }

    #[test]
    fn direction_helper_matches_gpu_packing() {
        let light = Light::directional();
        let transform = Mat4::from_rotation_x(0.7);

        let gpu = light.to_gpu(transform);
        let helper = light.direction(transform);

        assert!(
            (Vec3::new(gpu.direction[0], gpu.direction[1], gpu.direction[2]) - helper).length()
                < 1e-6
        );
    }

    #[test]
    fn default_gpu_light_is_harmless() {
        // 空槽位用默认值填充，强度为 0 时不会影响画面。
        let light = GpuLight::default();
        assert_eq!(light.color[3], 0.0);
    }
}
