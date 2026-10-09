//! 经验光照模型：three.js 的 `MeshPhongMaterial` / `MeshLambertMaterial`。
//!
//! 公式和 three.js 逐项对应，见 `phong.wgsl`。
//!
//! ```no_run
//! use kmath::Vec3;
//! use kpbr::phong::{PhongExt, PhongMaterial};
//!
//! let shiny = PhongMaterial::new(Vec3::splat(0.33)).with_shininess(80.0);
//! let matte = PhongMaterial::lambert(Vec3::new(0.8, 0.2, 0.2));
//! ```

use kasset::Resource;
use kmaterial::Material;
use kmath::{Vec3, Vec4};
use kshader::Shader;
use ktexture::Texture;
use std::sync::OnceLock;

/// Phong 的钩子源码。想在它上面再加自己的顶点钩子（位移、爆炸）时，
/// 把两段拼成一份着色器即可——光照和环境光两个钩子都在这里面。
pub const PHONG_WGSL: &str = include_str!("phong.wgsl");

/// Phong / Lambert 材质的构造。
///
/// 返回的是一个普通的 [`Material`]（挂了 Phong 的光照钩子），所以
/// 贴图、透明、双面这些照常设置。
#[derive(Debug, Clone, Copy)]
pub struct PhongMaterial;

/// 构造出来之后还能接着链式改的那几项。
pub trait PhongExt {
    /// 光泽度（three.js 的 `shininess`，默认 30）。越大高光越小越亮。
    fn with_shininess(self, shininess: f32) -> Self;
    /// 高光色（three.js 的 `specular`，默认 0x111111）。
    fn with_specular(self, color: Vec3) -> Self;
    /// 高光贴图：取它的 r 通道乘在高光色上（three.js 的 `specularMap`）。
    fn with_specular_map(self, texture: Resource<Texture>) -> Self;
    /// 棋盘格高光：按 UV × `scale` 在两种颜色之间交替。
    fn with_specular_checker(self, scale: f32, a: Vec3, b: Vec3) -> Self;
}

impl PhongMaterial {
    /// 全进程共用的钩子资源。
    pub fn shader() -> Resource<Shader> {
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        SHADER
            .get_or_init(|| {
                Resource::new_ok(
                    "builtin/phong.wgsl",
                    Shader::snippet(include_str!("phong.wgsl")),
                )
            })
            .clone()
    }

    /// three.js `MeshPhongMaterial` 的默认值：高光色 0x111111、光泽度 30。
    // 这几个材质模型是「拼一份 `Material`」的工厂，不是自己的类型——`new` 返回 `Material` 是有意的。
    #[allow(clippy::new_ret_no_self)]
    pub fn new(color: Vec3) -> Material {
        // 0x111111 的线性值。
        let specular = Vec3::splat(0.005_605);
        Material::standard()
            .with_name("phong")
            .with_shader(Self::shader())
            .with_base_color(color.extend(1.0))
            .with_metallic(0.0)
            .with_roughness(1.0)
            .with_param(0, specular.extend(30.0))
            .with_param(1, Vec4::ZERO)
    }

    /// three.js 的 `MeshLambertMaterial`：只有漫反射。
    pub fn lambert(color: Vec3) -> Material {
        Self::new(color).with_param(0, Vec4::new(0.0, 0.0, 0.0, 1.0))
    }
}

fn param(material: &Material, slot: usize) -> Vec4 {
    material
        .param(slot)
        .and_then(kmaterial::MaterialValue::as_vec4)
        .unwrap_or(Vec4::ZERO)
}

impl PhongExt for Material {
    fn with_shininess(self, shininess: f32) -> Self {
        let current = param(&self, 0);
        self.with_param(0, current.truncate().extend(shininess.max(1e-3)))
    }

    fn with_specular(self, color: Vec3) -> Self {
        let current = param(&self, 0);
        self.with_param(0, color.extend(current.w))
    }

    fn with_specular_map(self, texture: Resource<Texture>) -> Self {
        let current = param(&self, 1);
        self.with_custom_texture(0, texture)
            .with_param(1, Vec4::new(1.0, current.y, 0.0, 0.0))
    }

    fn with_specular_checker(self, scale: f32, a: Vec3, b: Vec3) -> Self {
        let current = param(&self, 1);
        self.with_param(1, Vec4::new(current.x, scale, 0.0, 0.0))
            .with_param(2, a.extend(0.0))
            .with_param(3, b.extend(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_match_three_js() {
        let material = PhongMaterial::new(Vec3::ONE);
        let p = param(&material, 0);
        assert!((p.w - 30.0).abs() < 1e-6);
        assert!(p.x > 0.0 && p.x < 0.01, "0x111111 的线性值约 0.0056");
    }

    #[test]
    fn lambert_has_no_highlight() {
        let material = PhongMaterial::lambert(Vec3::ONE);
        assert_eq!(param(&material, 0).truncate(), Vec3::ZERO);
    }

    #[test]
    fn setters_keep_the_other_half_of_the_slot() {
        let material = PhongMaterial::new(Vec3::ONE)
            .with_shininess(80.0)
            .with_specular(Vec3::X);
        assert_eq!(param(&material, 0), Vec4::new(1.0, 0.0, 0.0, 80.0));
    }

    #[test]
    fn the_hook_overrides_lighting_and_ambient() {
        let source = include_str!("phong.wgsl");
        assert!(source.contains("fn material_lighting"));
        assert!(source.contains("fn material_ambient"));
    }
}
