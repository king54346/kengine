//! 只接影子的材质：three.js 的 `ShadowMaterial`。
//!
//! 自己完全透明、不受光，只在影子落下的地方变暗。物体「站在」背景图、渐变天空、
//! 别的画面上时用它当地面：看不见地面本身，只看得见脚下的影子。
//!
//! ```no_run
//! use kmath::Vec3;
//! use kpbr::shadow_catcher::ShadowMaterial;
//!
//! let ground = ShadowMaterial::new(Vec3::new(0.36, 0.4, 0.41), 0.3);
//! ```
//!
//! 做法：光照钩子读 `LightingInput::visibility`（这盏灯在这一点的阴影可见度），
//! 把不透明度写成 `(1 − 可见度) × opacity`，返回 0——引擎取钩子跑完之后的不透明度。

use kasset::Resource;
use kmaterial::{BlendMode, Material};
use kmath::Vec3;
use kshader::Shader;
use std::sync::OnceLock;

const SHADOW_CATCHER_WGSL: &str = "
// params[0] = (影子的颜色 rgb, 最深处的不透明度)
fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    out.base_color = vec4<f32>(surface.params[0].rgb, 0.0);
    out.emissive = surface.params[0].rgb;
    out.metallic = 0.0;
    out.roughness = 1.0;
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    // 只认会投影的灯（不投影的灯 visibility 恒为 1，不影响结果）。
    let shadow = (1.0 - clamp(input.visibility, 0.0, 1.0)) * (*surface).params[0].w;
    (*surface).base_color.a = max((*surface).base_color.a, shadow);
    return vec3<f32>(0.0);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}
";

/// 只接影子材质的构造。
#[derive(Debug, Clone, Copy)]
pub struct ShadowMaterial;

impl ShadowMaterial {
    /// 着色器钩子（全进程一份）。
    pub fn shader() -> Resource<Shader> {
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        SHADER
            .get_or_init(|| {
                Resource::new_ok(
                    "builtin/shadow_catcher.wgsl",
                    Shader::snippet(SHADOW_CATCHER_WGSL),
                )
            })
            .clone()
    }

    /// 影子颜色 `color`（线性）、最深处不透明度 `opacity`（three.js 的 `opacity`）。
    #[allow(clippy::new_ret_no_self)]
    pub fn new(color: Vec3, opacity: f32) -> Material {
        Material::standard()
            .with_name("shadow catcher")
            .with_shader(Self::shader())
            .with_param(0, color.extend(opacity.clamp(0.0, 1.0)))
            .with_blend_mode(BlendMode::Alpha)
    }
}
