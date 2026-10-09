//! 水下表面材质：按水深吸收直射光、叠流动的焦散、水线附近变湿。
//!
//! ```ignore
//! let seabed = caustics::material(Vec4::new(0.76, 0.68, 0.5, 1.0), &CausticsSettings::default());
//! node.set_material(seabed);
//! ```
//!
//! 材质本身就是标准 PBR（基础色、粗糙度、贴图照旧），只是加了三个钩子：
//! 水面以上和普通材质一模一样，所以一座岛从山顶到海底可以只用一份材质。

use kasset::Resource;
use kmaterial::Material;
use kmath::{Vec3, Vec4};

const SHADER: &str = include_str!("caustics.wgsl");

/// 焦散设置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CausticsSettings {
    /// 海平面高度。
    pub sea_level: f32,
    /// 焦散强度（0 = 没有）。
    pub intensity: f32,
    /// 焦散花纹的尺度（米）。
    pub scale: f32,
    /// 水线以上多高算湿的（米）。
    pub wet_band: f32,
    /// 每米的吸收（红绿蓝），和海面的 [`crate::ocean::WaterLook::absorption`] 用同一组值才对得上。
    pub absorption: Vec3,
}

impl Default for CausticsSettings {
    fn default() -> Self {
        Self {
            sea_level: 0.0,
            intensity: 1.0,
            scale: 2.2,
            wet_band: 0.8,
            absorption: Vec3::new(0.45, 0.09, 0.06) * 0.6,
        }
    }
}

/// 一份带焦散的材质。
pub fn material(base_color: Vec4, settings: &CausticsSettings) -> Material {
    let mut material = Material::standard()
        .with_shader(Resource::new_ok(
            "kcomponents/caustics.wgsl",
            kshader::Shader::snippet(SHADER),
        ))
        .with_base_color(base_color);
    apply(&mut material, settings);
    material
}

/// 改已有材质的焦散参数（海平面、强度……）。
pub fn apply(material: &mut Material, settings: &CausticsSettings) {
    material.set_param(
        0,
        Vec4::new(
            settings.sea_level,
            settings.intensity,
            settings.scale,
            settings.wet_band,
        ),
    );
    material.set_param(1, settings.absorption.extend(0.0));
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shader_compiles_against_the_engine() {
        krender::validate_material_hook(super::SHADER).expect("caustics.wgsl 编不过");
    }
}
