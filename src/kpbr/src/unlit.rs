//! 不受光的材质：three.js 的 `MeshBasicMaterial`。
//!
//! 贴图墙、天空穹顶、VRML 里没有 `Material` 的形体（规范要求它们不受光）、
//! 调试用的纯色物体，都要「颜色是什么就画成什么」。
//!
//! 基础色、基础色贴图、**顶点色**都照常生效（三者相乘），透明混合也照常
//! ——变的只是光照被整个关掉了。
//!
//! ```no_run
//! use kmath::Vec4;
//! use kpbr::unlit::UnlitMaterial;
//!
//! let material = UnlitMaterial::new(Vec4::new(1.0, 0.5, 0.0, 1.0));
//! ```

use kasset::Resource;
use kmaterial::Material;
use kmath::Vec4;
use kshader::Shader;
use std::sync::OnceLock;

/// 不受光材质的构造。
#[derive(Debug, Clone, Copy)]
pub struct UnlitMaterial;

impl UnlitMaterial {
    /// 着色器钩子本身。全进程共用一份资源——每个材质各建一份的话，
    /// 渲染器会把它们当成不同的着色器，各编一条管线。
    pub fn shader() -> Resource<Shader> {
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        SHADER
            .get_or_init(|| Resource::new_ok("builtin/unlit.wgsl", Shader::snippet(include_str!("unlit.wgsl"))))
            .clone()
    }

    /// 一个颜色为 `color`（线性，`w` 是不透明度）的不受光材质。
    pub fn new(color: Vec4) -> Material {
        Material::standard()
            .with_name("unlit")
            .with_shader(Self::shader())
            .with_base_color(color)
            .with_metallic(0.0)
            .with_roughness(1.0)
    }

    /// 把一个现成的材质改成不受光，颜色、贴图、混合方式原样保留。
    pub fn apply(material: &mut Material) {
        material.set_shader(Self::shader());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_material_shares_one_shader_resource() {
        let a = UnlitMaterial::new(Vec4::ONE);
        let b = UnlitMaterial::new(Vec4::ZERO);
        assert_eq!(a.shader().unwrap().path(), b.shader().unwrap().path());
    }

    #[test]
    fn all_three_hooks_are_overridden() {
        let source = include_str!("unlit.wgsl");
        for hook in ["fn material_surface", "fn material_lighting", "fn material_ambient"] {
            assert!(source.contains(hook), "缺少 {hook}");
        }
    }
}
