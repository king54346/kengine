//! 材质钩子、后处理、计算着色器共用的程序噪声库（MaterialX 那一套，three.js TSL 的 `mx_*` 函数）。
//!
//! 不自动拼进每个着色器——多数材质用不上，白白拖慢管线编译。要用就拼在自己的源码前面：
//!
//! ```
//! use kshader::{Shader, noise};
//!
//! let hook = format!(
//!     "{}\nfn material_surface(s: Surface) -> Surface {{\n    var out = s;\n    out.base_color = vec4<f32>(vec3<f32>(mx_noise_float(s.world_position * 4.0) * 0.5 + 0.5), 1.0);\n    return out;\n}}",
//!     noise::WGSL
//! );
//! let shader = Shader::snippet(hook);
//! # let _ = shader;
//! ```
//!
//! 函数清单见 `noise.wgsl` 开头的表。

/// 噪声库的 WGSL 源码。
pub const WGSL: &str = include_str!("noise.wgsl");

#[cfg(test)]
mod tests {
    #[test]
    fn the_library_is_valid_wgsl_on_its_own() {
        let module = naga::front::wgsl::parse_str(super::WGSL)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(super::WGSL)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        for name in [
            "mx_noise_float",
            "mx_noise_float_2d",
            "mx_noise_vec3",
            "mx_fractal_noise_float",
            "mx_cell_noise_float",
            "mx_worley_noise_float",
            "tsl_hash",
            "tsl_remap",
            "tsl_tri_noise3d",
        ] {
            assert!(
                module
                    .functions
                    .iter()
                    .any(|(_, f)| f.name.as_deref() == Some(name)),
                "缺 {name}"
            );
        }
    }
}
