//! 程序化木纹材质（three.js 的 `WoodNodeMaterial`）：十种木头 × 四种表面处理的预设，也可以自己调参数。
//!
//! ```no_run
//! use kpbr::wood::{Finish, WoodGenus, WoodMaterial};
//!
//! // 预设：胡桃木、亮光漆。
//! let material = WoodMaterial::preset(WoodGenus::Walnut, Finish::Gloss).material();
//! // 自定义：在预设上改几项。
//! let custom = WoodMaterial { ring_thickness: 1.0 / 50.0, ..WoodMaterial::preset(WoodGenus::Teak, Finish::Raw) };
//! # let _ = (material, custom);
//! ```
//!
//! 纹理按**模型空间**坐标算，年轮轴是模型的 Z 轴：一块木板要让纹理顺着长边走，就让长边沿 Z。
//! [`offset`](WoodMaterial::offset) 平移采样坐标，同一种木头的几块板子各给一个就不会长得一模一样。
//!
//! 表面处理（漆）走 [`Physical`] 的清漆层。
//! 着色器很重（四层空间扭曲 + 一个 27 格的 Voronoi），适合展示用的物件，不适合铺满整个场景的地板。

use crate::physical::{PHYSICAL_WGSL, Physical};
use kasset::Resource;
use kmaterial::Material;
use kmath::{Vec3, Vec4};
use kshader::Shader;
use std::sync::OnceLock;

/// 木头种类（原版的 `WoodGenuses`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WoodGenus {
    /// 柚木。
    Teak,
    /// 胡桃木。
    Walnut,
    /// 白橡木。
    WhiteOak,
    /// 松木。
    Pine,
    /// 杨木。
    Poplar,
    /// 枫木。
    Maple,
    /// 红橡木。
    RedOak,
    /// 樱桃木。
    Cherry,
    /// 雪松。
    Cedar,
    /// 桃花心木。
    Mahogany,
}

impl WoodGenus {
    /// 全部十种，按原版的顺序。
    pub const ALL: [WoodGenus; 10] = [
        Self::Teak,
        Self::Walnut,
        Self::WhiteOak,
        Self::Pine,
        Self::Poplar,
        Self::Maple,
        Self::RedOak,
        Self::Cherry,
        Self::Cedar,
        Self::Mahogany,
    ];

    /// 原版的名字（`teak`、`white_oak`…）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Teak => "teak",
            Self::Walnut => "walnut",
            Self::WhiteOak => "white_oak",
            Self::Pine => "pine",
            Self::Poplar => "poplar",
            Self::Maple => "maple",
            Self::RedOak => "red_oak",
            Self::Cherry => "cherry",
            Self::Cedar => "cedar",
            Self::Mahogany => "mahogany",
        }
    }
}

/// 表面处理（原版的 `Finishes`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// 原木，不上漆。
    Raw,
    /// 哑光漆。
    Matte,
    /// 半亮光漆。
    Semigloss,
    /// 亮光漆。
    Gloss,
}

impl Finish {
    /// 全部四种，按原版的顺序。
    pub const ALL: [Finish; 4] = [Self::Raw, Self::Matte, Self::Semigloss, Self::Gloss];

    /// 原版的名字。
    pub fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Matte => "matte",
            Self::Semigloss => "semigloss",
            Self::Gloss => "gloss",
        }
    }

    /// (清漆强度, 清漆粗糙度, 木头压暗系数)。
    ///
    /// 压暗系数恒为 1：原版 `GetWoodPreset` 给每种处理算了一个 `clearcoatDarken`（亮光 0.2…），
    /// 但颜色节点乘的是**模块级**那份 teak / raw 预设的值，实际一律是 1。照原版的效果来；
    /// 想要漆面压暗就自己设 [`WoodMaterial::clearcoat_darken`]。
    fn coat(self) -> (f32, f32, f32) {
        match self {
            Self::Gloss => (1.0, 0.1, 1.0),
            Self::Semigloss => (1.0, 0.4, 1.0),
            Self::Matte => (1.0, 1.0, 1.0),
            Self::Raw => (0.0, 0.0, 1.0),
        }
    }
}

/// 木纹的全部参数。字段名和原版一致（蛇形）。颜色是线性的。
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub struct WoodMaterial {
    pub center_size: f32,
    pub large_warp_scale: f32,
    pub large_grain_stretch: f32,
    pub small_warp_strength: f32,
    pub small_warp_scale: f32,
    pub fine_warp_strength: f32,
    pub fine_warp_scale: f32,
    /// 年轮宽度（原版写成 `1 / 34` 这种）。
    pub ring_thickness: f32,
    pub ring_bias: f32,
    pub ring_size_variance: f32,
    pub ring_variance_scale: f32,
    pub bark_thickness: f32,
    pub splotch_scale: f32,
    pub splotch_intensity: f32,
    pub cell_scale: f32,
    pub cell_size: f32,
    pub dark_grain_color: Vec3,
    pub light_grain_color: Vec3,
    /// 清漆层强度。
    pub clearcoat: f32,
    /// 清漆层粗糙度。
    pub clearcoat_roughness: f32,
    /// 上了漆的木头压暗多少（1 = 不压）。
    pub clearcoat_darken: f32,
    /// 采样坐标的平移（原版 `transformationMatrix` 的平移部分）。
    pub offset: Vec3,
}

/// sRGB 十六进制颜色 → 线性。
fn hex(value: u32) -> Vec3 {
    let channel = |shift: u32| {
        let c = ((value >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Vec3::new(channel(16), channel(8), channel(0))
}

impl WoodMaterial {
    /// 一种木头、一种处理的预设（原版 `GetWoodPreset`）。
    pub fn preset(genus: WoodGenus, finish: Finish) -> Self {
        // (centerSize, largeWarpScale, largeGrainStretch, smallWarpStrength, smallWarpScale, fineWarpStrength, fineWarpScale,
        //  1/ringThickness, ringBias, ringSizeVariance, ringVarianceScale, barkThickness, splotchScale, splotchIntensity,
        //  cellScale, cellSize, dark, light)
        #[rustfmt::skip]
        let p: ([f32; 16], u32, u32) = match genus {
            WoodGenus::Teak => ([1.11, 0.32, 0.24, 0.059, 2.0, 0.006, 32.8, 34.0, 0.03, 0.03, 4.4, 0.3, 0.2, 0.541, 910.0, 0.1], 0x0c0504, 0x926c50),
            WoodGenus::Walnut => ([1.07, 0.42, 0.34, 0.016, 10.3, 0.028, 12.7, 32.0, 0.08, 0.03, 5.5, 0.98, 1.84, 0.97, 710.0, 0.31], 0x311e13, 0x523424),
            WoodGenus::WhiteOak => ([1.23, 0.21, 0.21, 0.034, 2.44, 0.01, 14.3, 34.0, 0.82, 0.16, 1.4, 0.7, 0.2, 0.541, 800.0, 0.28], 0x8b4c21, 0xc57e43),
            WoodGenus::Pine => ([1.23, 0.21, 0.18, 0.041, 2.44, 0.006, 23.2, 24.0, 0.1, 0.07, 5.0, 0.35, 0.51, 3.32, 1480.0, 0.07], 0xc58355, 0xd19d61),
            WoodGenus::Poplar => ([1.43, 0.33, 0.18, 0.04, 4.3, 0.004, 33.6, 37.0, 0.07, 0.03, 3.8, 0.3, 1.92, 0.71, 830.0, 0.04], 0x716347, 0x998966),
            WoodGenus::Maple => ([1.4, 0.38, 0.25, 0.067, 2.5, 0.005, 33.6, 35.0, 0.1, 0.07, 4.6, 0.61, 0.46, 1.49, 800.0, 0.03], 0xb08969, 0xbc9d7d),
            WoodGenus::RedOak => ([1.21, 0.24, 0.25, 0.044, 2.54, 0.01, 14.5, 34.0, 0.92, 0.03, 5.6, 1.01, 0.28, 3.48, 800.0, 0.25], 0xaf613b, 0xe0a27a),
            WoodGenus::Cherry => ([1.33, 0.11, 0.33, 0.024, 2.48, 0.01, 15.3, 36.0, 0.02, 0.04, 6.5, 0.09, 1.27, 1.24, 1530.0, 0.15], 0x913f27, 0xb45837),
            WoodGenus::Cedar => ([1.11, 0.39, 0.12, 0.061, 1.9, 0.006, 4.8, 25.0, 0.01, 0.07, 6.7, 0.1, 0.61, 2.54, 630.0, 0.19], 0x9a5b49, 0xae745e),
            WoodGenus::Mahogany => ([1.25, 0.26, 0.29, 0.044, 2.54, 0.01, 15.3, 38.0, 0.01, 0.33, 1.2, 0.07, 0.77, 1.39, 1400.0, 0.23], 0x501d12, 0x6d3722),
        };
        let (
            [
                center_size,
                large_warp_scale,
                large_grain_stretch,
                small_warp_strength,
                small_warp_scale,
                fine_warp_strength,
                fine_warp_scale,
                rings,
                ring_bias,
                ring_size_variance,
                ring_variance_scale,
                bark_thickness,
                splotch_scale,
                splotch_intensity,
                cell_scale,
                cell_size,
            ],
            dark,
            light,
        ) = p;
        let (clearcoat, clearcoat_roughness, clearcoat_darken) = finish.coat();
        Self {
            center_size,
            large_warp_scale,
            large_grain_stretch,
            small_warp_strength,
            small_warp_scale,
            fine_warp_strength,
            fine_warp_scale,
            ring_thickness: 1.0 / rings,
            ring_bias,
            ring_size_variance,
            ring_variance_scale,
            bark_thickness,
            splotch_scale,
            splotch_intensity,
            cell_scale,
            cell_size,
            dark_grain_color: hex(dark),
            light_grain_color: hex(light),
            clearcoat,
            clearcoat_roughness,
            clearcoat_darken,
            offset: Vec3::ZERO,
        }
    }

    /// 换一种表面处理（清漆三项一起换）。
    pub fn with_finish(mut self, finish: Finish) -> Self {
        (
            self.clearcoat,
            self.clearcoat_roughness,
            self.clearcoat_darken,
        ) = finish.coat();
        self
    }

    /// 改采样坐标的平移。
    pub fn with_offset(mut self, offset: Vec3) -> Self {
        self.offset = offset;
        self
    }

    /// 建一份材质。
    pub fn material(&self) -> Material {
        let mut material = Material::default().with_metallic(0.0).with_roughness(1.0);
        self.apply(&mut material);
        material
    }

    /// 把参数写进一份已有的材质（换着色器、写 params[0..4] 和 [8..14]）。改了参数每帧调也行，不重编着色器。
    pub fn apply(&self, material: &mut Material) {
        Physical {
            clearcoat: self.clearcoat,
            clearcoat_roughness: self.clearcoat_roughness,
            ..Physical::default()
        }
        .apply(material);
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        material.set_shader(
            SHADER
                .get_or_init(|| {
                    Resource::new_ok(
                        "builtin/wood.wgsl",
                        Shader::snippet(format!(
                            "{}\n{PHYSICAL_WGSL}\n{}",
                            kshader::noise::WGSL,
                            include_str!("wood.wgsl")
                        )),
                    )
                })
                .clone(),
        );
        material.set_param(
            8,
            Vec4::new(
                self.center_size,
                self.large_warp_scale,
                self.large_grain_stretch,
                self.small_warp_strength,
            ),
        );
        material.set_param(
            9,
            Vec4::new(
                self.small_warp_scale,
                self.fine_warp_strength,
                self.fine_warp_scale,
                self.ring_thickness,
            ),
        );
        material.set_param(
            10,
            Vec4::new(
                self.ring_bias,
                self.ring_size_variance,
                self.ring_variance_scale,
                self.bark_thickness,
            ),
        );
        material.set_param(
            11,
            Vec4::new(
                self.splotch_scale,
                self.splotch_intensity,
                self.cell_scale,
                self.cell_size,
            ),
        );
        material.set_param(12, self.dark_grain_color.extend(self.clearcoat_darken));
        material.set_param(13, self.light_grain_color.extend(0.0));
        material.set_param(14, self.offset.extend(0.0));
    }
}

impl Default for WoodMaterial {
    /// 柚木、原木（原版的默认）。
    fn default() -> Self {
        Self::preset(WoodGenus::Teak, Finish::Raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_match_the_original_numbers() {
        let teak = WoodMaterial::preset(WoodGenus::Teak, Finish::Raw);
        assert!((teak.ring_thickness - 1.0 / 34.0).abs() < 1e-7);
        assert_eq!((teak.clearcoat, teak.clearcoat_darken), (0.0, 1.0));
        let gloss = WoodMaterial::preset(WoodGenus::Mahogany, Finish::Gloss);
        assert_eq!(
            (
                gloss.clearcoat,
                gloss.clearcoat_roughness,
                gloss.clearcoat_darken
            ),
            (1.0, 0.1, 1.0)
        );
        // 颜色换到了线性空间：0x0c = 12 → 约 0.0037。
        assert!(
            (teak.dark_grain_color.x - 0.003_677).abs() < 1e-4,
            "{:?}",
            teak.dark_grain_color
        );
    }

    #[test]
    fn the_material_writes_its_parameters_above_the_physical_ones() {
        let material = WoodMaterial::preset(WoodGenus::Pine, Finish::Matte).material();
        let param = |slot: usize| {
            material
                .param(slot)
                .and_then(|value| value.as_vec4())
                .unwrap()
        };
        assert_eq!(param(11).z, 1480.0);
        // 物理材质的清漆参数还在第 3 槽。
        assert_eq!((param(3).z, param(3).w), (1.0, 1.0));
    }
}
