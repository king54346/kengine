//! 线框材质：three.js 的 `material.wireframe = true`。
//!
//! 和 [`points`](crate::points) 一样，是引擎自带的一份材质钩子。配套几何
//! 由 `kmesh::Mesh::wireframe` 生成——普通网格挂上这个材质会画成**实心**
//! （`uv1` 全是零，每个片元都被当成「正好在边上」留下来）。
//!
//! ```no_run
//! use kmath::Vec3;
//! use kpbr::wireframe::WireframeMaterial;
//!
//! // 几何那一半：`kmesh::Mesh::icosphere(4).wireframe()`。
//! let material = WireframeMaterial::new(Vec3::ONE, 1.0);
//! ```
//!
//! 线条**照常受光**（它仍然是标准 PBR 表面，只是中间被挖空了），
//! 所以默认是双面的：挖空之后背面的线从正面的空隙里看得见，
//! 不画背面的话球的后半边会凭空消失，和 three.js 的观感不一致。

use kasset::Resource;
use kmaterial::Material;
use kmath::{Vec3, Vec4};
use kshader::Shader;
use std::sync::OnceLock;

/// 线框材质的构造与参数。
#[derive(Debug, Clone, Copy)]
pub struct WireframeMaterial;

impl WireframeMaterial {
    /// 建一个线框材质。`width` 是线宽（屏幕像素）。
    pub fn new(color: Vec3, width: f32) -> Material {
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        Material::standard()
            .with_name("wireframe")
            .with_shader(
                SHADER
                    .get_or_init(|| {
                        Resource::new_ok(
                            "builtin/wireframe.wgsl",
                            Shader::snippet(include_str!("wireframe.wgsl")),
                        )
                    })
                    .clone(),
            )
            .with_base_color(color.extend(1.0))
            .with_metallic(0.0)
            .with_roughness(1.0)
            .with_double_sided()
            .with_param(0, Vec4::new(width.max(0.01), 0.0, 0.0, 0.0))
    }

    /// 改线宽（像素）。
    pub fn set_width(material: &mut Material, width: f32) {
        material.set_param(0, Vec4::new(width.max(0.01), 0.0, 0.0, 0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_is_a_surface_hook() {
        // 完整拼接后的校验在 `tests/loader_imports.rs` 里（片段单独编不过）。
        assert!(include_str!("wireframe.wgsl").contains("fn material_surface"));
    }

    #[test]
    fn width_lands_in_the_first_param_and_never_hits_zero() {
        let mut material = WireframeMaterial::new(Vec3::ONE, 2.0);
        assert_eq!(material.param(0).unwrap().as_vec4().unwrap().x, 2.0);
        WireframeMaterial::set_width(&mut material, 0.0);
        assert!(material.param(0).unwrap().as_vec4().unwrap().x > 0.0);
        assert!(material.double_sided());
    }
}
