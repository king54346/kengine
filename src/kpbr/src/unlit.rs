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

/// 只关光照的那两个钩子（直射、环境都返回 0），不含 `material_surface`。
///
/// 自己写表面钩子（程序化图案、网格地面）又想要「不受光」时，把它拼在
/// 自己的钩子后面，颜色写进 `emissive`、基础色清零即可。
pub const UNLIT_LIGHTING_WGSL: &str = "
fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}
";

/// 不受光材质的构造。
#[derive(Debug, Clone, Copy)]
pub struct UnlitMaterial;

impl UnlitMaterial {
    /// 着色器钩子本身。全进程共用一份资源——每个材质各建一份的话，
    /// 渲染器会把它们当成不同的着色器，各编一条管线。
    pub fn shader() -> Resource<Shader> {
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        SHADER
            .get_or_init(|| {
                Resource::new_ok(
                    "builtin/unlit.wgsl",
                    Shader::snippet(include_str!("unlit.wgsl")),
                )
            })
            .clone()
    }

    /// 一个颜色为 `color`（线性，`w` 是不透明度）的不受光材质。
    // 这几个材质模型是「拼一份 `Material`」的工厂，不是自己的类型——`new` 返回 `Material` 是有意的。
    #[allow(clippy::new_ret_no_self)]
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
    fn the_lighting_only_hooks_match_the_full_shader() {
        // 两份得一致：拼出来的「自定义表面 + 不受光」和内置不受光材质
        // 的光照行为应该是同一回事。
        let full = include_str!("unlit.wgsl");
        for line in UNLIT_LIGHTING_WGSL.lines().filter(|l| l.starts_with("fn ")) {
            assert!(full.contains(line), "unlit.wgsl 里没有 {line}");
        }
    }

    #[test]
    fn every_material_shares_one_shader_resource() {
        let a = UnlitMaterial::new(Vec4::ONE);
        let b = UnlitMaterial::new(Vec4::ZERO);
        assert_eq!(a.shader().unwrap().path(), b.shader().unwrap().path());
    }

    #[test]
    fn all_three_hooks_are_overridden() {
        let source = include_str!("unlit.wgsl");
        for hook in [
            "fn material_surface",
            "fn material_lighting",
            "fn material_ambient",
        ] {
            assert!(source.contains(hook), "缺少 {hook}");
        }
    }
}
