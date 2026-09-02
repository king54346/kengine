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
pub mod cluster;
pub mod shadow;

use bytemuck::{Pod, Zeroable};
use kmath::{Mat4, Vec3};

/// 前向渲染一次能处理的光源上限。
///
/// 超出的光源会被丢弃——受 uniform 缓冲大小与着色器循环开销限制。
/// 要支持更多光源需要改用延迟渲染或分簇前向渲染。
pub const MAX_LIGHTS: usize = 256;

/// Cook-Torrance 光照求值的 WGSL 源码，由渲染器拼进着色器。
pub const LIGHT_WGSL: &str = include_str!("light.wgsl");

/// 聚簇下标的 WGSL 实现。
///
/// 和 [`cluster::ClusterGrid`] **必须算出同一个下标**。对不上的话片元
/// 读到的是别的簇的名单——光照在屏幕上整体错位一块，而且不越界、
/// 不报错、不掉帧。两边都放在这个 crate 里，就是为了让那条对拍测试
/// 只编译这一段而不必把整套光照拖进来。
pub const CLUSTER_WGSL: &str = include_str!("cluster.wgsl");

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
    /// 矩形面光源：一块会发光的矩形板子。
    ///
    /// 和点光源的区别是**它有面积**：高光会拉成一条，边缘的阴影是软的，
    /// 而不是点光源那种针尖一样的亮点。窗户、灯箱、屏幕都是这个形状。
    ///
    /// 朝向是所在节点的 -Z，宽沿 +X、高沿 +Y。**只向正面发光**——
    /// 背面是黑的，和真实的灯箱一样。
    Rect {
        /// 宽（沿节点的 X 轴）。
        width: f32,
        /// 高（沿节点的 Y 轴）。
        height: f32,
        /// 作用半径。
        range: f32,
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
            Self::Rect { .. } => 4.0,
        }
    }

    /// 作用半径；方向光返回 0（不参与衰减）。
    pub fn range(&self) -> f32 {
        match self {
            Self::Directional | Self::Hemisphere { .. } => 0.0,
            Self::Rect { range, .. } => *range,
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
    /// 这盏灯照亮哪些**层**。位掩码，和节点的
    /// [`light_mask`](kscene::Node::light_mask) 按位与，非零才照亮。
    ///
    /// 默认是全 1（照亮一切）。分层的用处是「这盏灯只打在角色身上」
    /// 「这盏灯只照场景不照角色」——美术调光时几乎必用，而用「把灯挪远」
    /// 之类的物理手段去凑永远凑不准。
    ///
    /// 灯和物体**两边都得同意**：任一方把对方的层关掉就不照。
    pub mask: u32,
    /// 投影贴图（cookie / gobo）在**场景的 cookie 图集**里的层号，**加一**。
    ///
    /// `0` 表示不投图案。加一是为了让「没设」和「用第 0 层」分得开——
    /// 用 `Option<u32>` 的话每盏灯要多占 8 字节，而这是要传上显存的。
    ///
    /// 图集由 [`Scene::set_cookie_atlas`](kscene::Scene::set_cookie_atlas)
    /// 登记，必须是一张多层纹理（[`Texture::from_layers`](ktexture::Texture::from_layers)）。
    ///
    /// **只对聚光灯有意义**。点光源没有朝向，投不出图案；
    /// 方向光的投影是正交的，那要另一套矩阵。
    pub cookie: u32,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            kind: LightKind::Directional,
            color: Vec3::ONE,
            intensity: 3.0,
            enabled: true,
            cast_shadows: false,
            // 全 1：默认照亮一切。默认给 1 的话，
            // 「没设过掩码的灯」和「只在第 0 层的灯」就分不开了。
            mask: u32::MAX,
            cookie: 0,
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
            Self::Rect { .. } => 4,
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
                4 => Self::Rect {
                    width: 0.0,
                    height: 0.0,
                    range: 0.0,
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
            Self::Rect {
                width,
                height,
                range,
            } => {
                width.visit("Width", &mut region)?;
                height.visit("Height", &mut region)?;
                range.visit("Range", &mut region)?;
            }
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

        // 掩码是后加的字段。老存档里没有这块区域，读不到就当「照亮一切」
        // ——为了一个位掩码让整个场景读不进来，不划算。
        //
        // 但**不能不存**：掩码是美术调出来的数据，丢了之后「这盏灯只照
        // 角色」会变成「照亮一切」，读档之后画面就不一样了。
        let mut mask = self.mask;
        if mask.visit("Mask", &mut region).is_ok() {
            self.mask = mask;
        } else if region.is_reading() {
            self.mask = u32::MAX;
        }

        // cookie 同理：后加的字段，老存档读不到就当没有。
        let mut cookie = self.cookie;
        if cookie.visit("Cookie", &mut region).is_ok() {
            self.cookie = cookie;
        } else if region.is_reading() {
            self.cookie = 0;
        }
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

    /// 矩形面光源：一块会发光的板子。
    ///
    /// 朝向是所在节点的 -Z，宽沿 +X、高沿 +Y。**只向正面发光**。
    ///
    /// 和点光源的区别是它**有面积**：高光会拉成一条而不是一个亮点，
    /// 这是窗户、灯箱、屏幕看起来对不对的关键。
    ///
    /// 强度默认给 10：面光源的能量摊在整块面积上，按点光源那个 20
    /// 会亮得过头。
    pub fn rect(width: f32, height: f32, range: f32) -> Self {
        Self {
            kind: LightKind::Rect {
                width: width.max(1e-4),
                height: height.max(1e-4),
                range: range.max(0.0),
            },
            intensity: 10.0,
            ..Default::default()
        }
    }

    /// 半球光：从天空和地面两个方向来的环境光。
    ///
    /// `ground_color` 是地面反射的颜色，天空的颜色用
    /// [`with_color`](Self::with_color) 给。
    ///
    /// 户外场景里它比一盏方向光顶用——只有太阳的话背光面是纯黑的，
    /// 而现实里那一面被天空和地面照亮着。强度默认给 1 而不是点光源那种
    /// 20：它是**环境**项，直接乘在反照率上，给大了整个画面会白掉。
    pub fn hemisphere(ground_color: Vec3) -> Self {
        Self {
            kind: LightKind::Hemisphere { ground_color },
            intensity: 1.0,
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

    /// 给这盏聚光灯挂一张投影贴图（cookie / gobo）。
    ///
    /// `layer` 是场景 cookie 图集里的层号（从 0 数）。传 [`None`] 摘掉。
    ///
    /// ```
    /// # use klight::Light;
    /// let gobo = Light::spot(10.0, 20.0, 30.0).with_cookie(Some(2));
    /// // 内部存的是「层号 + 1」，0 留给「没有 cookie」。
    /// assert_eq!(gobo.cookie, 3);
    /// ```
    pub fn with_cookie(mut self, layer: Option<u32>) -> Self {
        self.cookie = layer.map_or(0, |index| index + 1);
        self
    }

    /// 指定这盏灯照亮哪些层。见 [`mask`](Self::mask)。
    ///
    /// ```
    /// # use klight::Light;
    /// // 只照第 0 层——比如「只打在角色身上的补光」。
    /// let key = Light::point(6.0).with_mask(0b0001);
    /// assert_eq!(key.mask, 1);
    /// ```
    pub fn with_mask(mut self, mask: u32) -> Self {
        self.mask = mask;
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
            // 矩形面光源没有内外锥，那两个槽位正好放半宽半高。
            // 存半边长而不是全长：着色器算四个角时用的就是半边长，
            // 每个片元省两次除法。
            LightKind::Rect { width, height, .. } => [width * 0.5, height * 0.5, 0.0, 0.0],
            _ => [cos_inner, cos_outer, 0.0, 0.0],
        };

        // 右轴：节点的 +X。非均匀缩放下它未必和方向正交，
        // 但归一化之后足够——着色器那边会拿它和方向叉出上轴，
        // 真正歪掉要等到有人给灯加剪切变换，那时该修的是那个变换。
        let right = world_transform
            .x_axis
            .truncate()
            .normalize_or(Vec3::X);
        // 聚光灯的外锥正切，cookie 投影要它把方向转成 UV。
        // 在这里算一次，省掉每个片元一次 `tan`。
        let tan_outer = match self.kind {
            LightKind::Spot { outer_angle, .. } => outer_angle.to_radians().tan(),
            _ => 0.0,
        };

        GpuLight {
            position: [position.x, position.y, position.z, self.kind.tag()],
            direction: [direction.x, direction.y, direction.z, self.kind.range()],
            color: [self.color.x, self.color.y, self.color.z, self.intensity],
            params,
            extra: [self.mask, self.cookie, 0, 0],
            right: [right.x, right.y, right.z, tan_outer],
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
    /// x = 照亮哪些层的位掩码，y = cookie 层号（0 = 没有），其余保留。
    ///
    /// 单开一个 `u32` 的 vec4 而不是把掩码塞进 `params.w`：
    /// 掩码和层号都是**整数**，从 `f32` 里 bitcast 出来能用但读起来像在耍花招。
    pub extra: [u32; 4],
    /// xyz = 光源的世界右轴（已归一化），w = 聚光灯的 `tan(外锥角)`。
    ///
    /// # 为什么要存右轴
    ///
    /// 两个地方要它，而且都**没法从方向反推**：
    ///
    /// - **cookie 的朝向**：一张投影图案是有上下左右的，绕光轴转一下
    ///   图案就该跟着转。只有方向的话就只能拿世界上方去凑一个基，
    ///   而那样图案永远转不了。
    /// - **矩形面光源的平面**：宽沿右轴、高沿上轴，上轴由方向叉右轴得到。
    ///
    /// 多这 16 字节 × 256 盏灯是 4 KB，可以忽略。
    pub right: [f32; 4],
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
        // 6 个 vec4 = 96 字节；与 light.wgsl 的 Light 结构一致。
        // 对不上不会报错，只会让着色器逐盏读到错位的字段——
        // 画面上是一堆位置乱七八糟的灯。
        assert_eq!(size_of::<GpuLight>(), 96);
        assert_eq!(size_of::<GpuLight>() % 16, 0);
    }

    #[test]
    fn a_hemisphere_light_is_an_ambient_term_not_a_lamp() {
        // 强度默认给 1 而不是点光源那种 20：它直接乘在反照率上，
        // 给大了整个画面会白掉。
        let light = Light::hemisphere(Vec3::splat(0.2));
        assert_eq!(light.intensity, 1.0);
        // 没有位置也没有范围，所以不参与聚簇。
        assert_eq!(light.kind.range(), 0.0);
    }

    #[test]
    fn a_rect_light_stores_half_extents() {
        // 存半边长而不是全长：着色器算四个角时用的就是半边长，
        // 每个片元省两次除法。
        let light = Light::rect(4.0, 6.0, 20.0);
        let gpu = light.to_gpu(Mat4::IDENTITY);

        assert_eq!(gpu.params[0], 2.0);
        assert_eq!(gpu.params[1], 3.0);
        assert_eq!(gpu.direction[3], 20.0, "作用半径该在 direction.w 里");
    }

    #[test]
    fn the_right_axis_follows_the_node_rotation() {
        // cookie 的图案和矩形面光源的平面都靠它定向。
        // 从方向反推的话，绕光轴转一下图案不会跟着转。
        use kmath::Quat;
        let turned = Mat4::from_quat(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        let gpu = Light::spot(10.0, 10.0, 20.0).to_gpu(turned);

        // 绕 Y 转 90°：+X 轴转到 -Z。
        assert!((gpu.right[0] - 0.0).abs() < 1e-5, "{:?}", gpu.right);
        assert!((gpu.right[2] + 1.0).abs() < 1e-5, "{:?}", gpu.right);
    }

    #[test]
    fn the_spot_tangent_is_precomputed() {
        // 在 CPU 上算一次，省掉每个片元一次 `tan`。
        let gpu = Light::spot(10.0, 10.0, 45.0).to_gpu(Mat4::IDENTITY);
        assert!((gpu.right[3] - 1.0).abs() < 1e-4, "tan(45°) 该是 1");
    }

    #[test]
    fn a_cookie_layer_is_stored_plus_one() {
        // 加一是为了让「没设」和「用第 0 层」分得开。
        assert_eq!(Light::spot(1.0, 1.0, 1.0).cookie, 0);
        assert_eq!(Light::spot(1.0, 1.0, 1.0).with_cookie(Some(0)).cookie, 1);
        assert_eq!(Light::spot(1.0, 1.0, 1.0).with_cookie(Some(7)).cookie, 8);
        assert_eq!(Light::spot(1.0, 1.0, 1.0).with_cookie(None).cookie, 0);
    }

    #[test]
    fn a_light_lights_everything_by_default() {
        // 默认给 1 而不是全 1 的话，「没设过掩码的灯」和
        // 「只在第 0 层的灯」就分不开了。
        assert_eq!(Light::default().mask, u32::MAX);
    }

    #[test]
    fn the_mask_survives_the_trip_to_the_gpu() {
        let light = Light {
            mask: 0b1010,
            ..Light::default()
        };
        assert_eq!(light.to_gpu(Mat4::IDENTITY).extra[0], 0b1010);
    }

    #[test]
    fn masks_overlap_rather_than_match() {
        // 用「与非零」而不是「相等」：相等的话每盏灯只能属于一层，
        // 「照亮角色和道具、但不照场景」就写不出来了。
        let light_mask = 0b0011u32;
        assert_ne!(light_mask & 0b0001, 0, "该照亮第 0 层");
        assert_ne!(light_mask & 0b0010, 0, "也该照亮第 1 层");
        assert_eq!(light_mask & 0b0100, 0, "不该照亮第 2 层");
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
