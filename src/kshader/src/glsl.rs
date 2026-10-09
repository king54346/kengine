//! ShaderToy 风格的 GLSL → WGSL。
//!
//! 网上成千上万的片段着色器是 ShaderToy 的写法：一个 `mainImage(out vec4 fragColor, in vec2 fragCoord)`，
//! 读 `iTime`、`iResolution` 这些全局量。three.js 有一个 GLSL → TSL 的转译器专门吃这种代码；
//! 这里走 naga 的 GLSL 前端：解析、校验，再用 WGSL 后端写回文本，拼进材质钩子或后处理里用。
//!
//! ```
//! let wgsl = kshader::glsl::shadertoy_to_wgsl(
//!     "void mainImage(out vec4 fragColor, in vec2 fragCoord) {\n\
//!          fragColor = vec4(fragCoord / iResolution.xy, 0.5 + 0.5 * sin(iTime), 1.0);\n\
//!      }",
//!     "toy",
//! )
//! .unwrap();
//! // 生成的入口：toy_image(fragCoord, 分辨率, 时间) -> vec4<f32>
//! assert!(wgsl.contains("fn toy_image("));
//! ```
//!
//! # 生成了什么
//!
//! - 原代码里所有函数、全局量都加上 `前缀_`，同一个材质里放两段 ShaderToy（各自都叫 `noise`）不会撞名，
//!   也不会撞上引擎着色器里的名字。
//! - ShaderToy 的全局量（`iTime`、`iResolution`、`iTimeDelta`、`iFrame`、`iMouse`、`iDate`）变成
//!   `var<private>`，由生成的 `前缀_image(frag_coord, resolution, time)` 每次调用前填好再调 `mainImage`。
//!   `iMouse` / `iDate` 恒为 0（想要的话自己在调用前写 `前缀_iMouse`）。
//! - `fragCoord` 的约定和 ShaderToy 一样：像素坐标，**原点在左下**。
//!
//! # 不支持
//!
//! `iChannel0..3`（纹理输入）和多 pass（Buffer A/B…）。naga 的 GLSL 前端也有自己的限制
//! （个别内建函数、数组的某些写法），解析失败时错误信息原样带回来。

use crate::ShaderError;

/// ShaderToy 的全局量：（GLSL 里的名字, 类型）。
const UNIFORMS: [(&str, &str); 6] = [
    ("iResolution", "vec3"),
    ("iTime", "float"),
    ("iTimeDelta", "float"),
    ("iFrame", "int"),
    ("iMouse", "vec4"),
    ("iDate", "vec4"),
];

/// 把一段普通的 GLSL（一堆函数、常量、结构体，不需要 `main`）转成 WGSL，名字都带上 `prefix_`。
///
/// three.js 的 GLSL → WGSL / TSL 转译器那个用途：拿网上的 GLSL 噪声、SDF 函数库来用。
/// `prefix` 为空时不加前缀（拼进同一个着色器的只有这一段时可以这样）。
///
/// ```
/// let wgsl = kshader::glsl::to_wgsl("float twice(float x) { return x * 2.0; }", "").unwrap();
/// assert!(wgsl.contains("fn twice("));
/// ```
pub fn to_wgsl(source: &str, prefix: &str) -> Result<String, ShaderError> {
    let mut glsl = String::from(
        "#version 450
#line 1
",
    );
    glsl.push_str(source);
    glsl.push_str(
        "
void main() {}
",
    );
    convert(&glsl, prefix)
}

/// 解析、加前缀、校验、写回 WGSL。`glsl` 是带 `#version` 和空 `main` 的完整片元着色器。
fn convert(glsl: &str, prefix: &str) -> Result<String, ShaderError> {
    if !prefix.is_empty()
        && (prefix.starts_with(|c: char| c.is_ascii_digit())
            || !prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        return Err(ShaderError::Parse(format!(
            "前缀「{prefix}」不是合法的标识符"
        )));
    }
    let rename = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}_{name}")
        }
    };
    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(naga::ShaderStage::Fragment);
    let mut module = frontend
        .parse(&options, glsl)
        .map_err(|error| ShaderError::Parse(error.emit_to_string(glsl)))?;
    module.entry_points.clear();
    for (_, function) in module.functions.iter_mut() {
        if let Some(name) = &function.name {
            function.name = Some(rename(name));
        }
    }
    for (_, global) in module.global_variables.iter_mut() {
        if let Some(name) = &global.name {
            global.name = Some(rename(name));
        }
    }
    for (_, constant) in module.constants.iter_mut() {
        if let Some(name) = &constant.name {
            constant.name = Some(rename(name));
        }
    }
    let renamed: Vec<_> = module
        .types
        .iter()
        .filter(|(_, ty)| ty.name.is_some() && matches!(ty.inner, naga::TypeInner::Struct { .. }))
        .map(|(handle, ty)| (handle, ty.clone()))
        .collect();
    for (handle, mut ty) in renamed {
        ty.name = ty.name.map(|name| rename(&name));
        module.types.replace(handle, ty);
    }
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| ShaderError::Validation(error.emit_to_string(glsl)))?;
    naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|error| ShaderError::Validation(error.to_string()))
}

/// 把一段 ShaderToy GLSL（定义了 `mainImage` 的那种）转成 WGSL 函数，名字都带上 `prefix_`。
///
/// 用法见模块文档。`prefix` 只能是字母、数字和下划线，不以数字开头。
pub fn shadertoy_to_wgsl(source: &str, prefix: &str) -> Result<String, ShaderError> {
    if prefix.is_empty()
        || prefix.starts_with(|c: char| c.is_ascii_digit())
        || !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ShaderError::Parse(format!(
            "前缀「{prefix}」不是合法的标识符"
        )));
    }
    // 全局量写成不带限定符的全局变量（naga 当 private 处理）；再补一个空 main 让前端认它是片段着色器。
    let mut glsl = String::from("#version 450\n");
    for (name, ty) in UNIFORMS {
        glsl.push_str(&format!("{ty} {name};\n"));
    }
    glsl.push_str("#line 1\n");
    glsl.push_str(source);
    glsl.push_str("\nvoid main() {}\n");

    if !source.contains("mainImage") {
        return Err(ShaderError::Parse(
            "ShaderToy 代码里没有 mainImage 函数".into(),
        ));
    }
    let mut wgsl = convert(&glsl, prefix)?;

    // `out vec4 fragColor` 在 WGSL 里是 `ptr<function, vec4<f32>>` 参数。
    wgsl.push_str(&format!(
        "\nfn {prefix}_image(frag_coord: vec2<f32>, resolution: vec2<f32>, time: f32) -> vec4<f32> {{\n    \
         {prefix}_iResolution = vec3<f32>(resolution, 1.0);\n    \
         {prefix}_iTime = time;\n    \
         var color = vec4<f32>(0.0);\n    \
         {prefix}_mainImage(&color, frag_coord);\n    \
         return color;\n}}\n"
    ));
    Ok(wgsl)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成的 WGSL 拼上一个片段入口，整体能过 naga 的 WGSL 解析和校验。
    fn compiles(wgsl: &str) {
        let full = format!(
            "{wgsl}\n@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {{ return toy_image(p.xy, vec2<f32>(640.0, 480.0), 1.0); }}\n"
        );
        let module = naga::front::wgsl::parse_str(&full)
            .unwrap_or_else(|error| panic!("{}\n{full}", error.emit_to_string(&full)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }

    #[test]
    fn a_typical_shadertoy_becomes_valid_wgsl() {
        let wgsl = shadertoy_to_wgsl(
            "float rand(vec2 co) { return fract(sin(dot(co, vec2(12.9898, 78.233))) * 43758.5453); }\n\
             float pnoise(vec2 co, int steps) {\n  float value = 0.0;\n  for (int i = 0; i < steps; i++) { value += rand(co * float(i)); }\n  return value / float(steps);\n}\n\
             void mainImage(out vec4 fragColor, in vec2 fragCoord) {\n  vec2 uv = fragCoord.xy / iResolution.xy;\n  fragColor = vec4(vec3(pnoise(uv, 5)), 1.0) * (0.5 + 0.5 * sin(iTime));\n}",
            "toy",
        )
        .unwrap();
        assert!(wgsl.contains("fn toy_rand("), "{wgsl}");
        assert!(!wgsl.contains("fn rand("), "没加前缀：{wgsl}");
        compiles(&wgsl);
    }

    #[test]
    fn two_shadertoys_with_the_same_function_names_do_not_collide() {
        let source = "float noise(vec2 p) { return fract(sin(p.x) * 1e4); }\nvoid mainImage(out vec4 c, in vec2 f) { c = vec4(noise(f)); }";
        let a = shadertoy_to_wgsl(source, "a").unwrap();
        let b = shadertoy_to_wgsl(source, "b").unwrap();
        let both = format!(
            "{a}\n{b}\n@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {{ return a_image(p.xy, vec2<f32>(1.0), 0.0) + b_image(p.xy, vec2<f32>(1.0), 0.0); }}\n"
        );
        naga::front::wgsl::parse_str(&both)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&both)));
    }

    #[test]
    fn errors_point_at_the_glsl() {
        assert!(matches!(
            shadertoy_to_wgsl(
                "void mainImage(out vec4 c, in vec2 f) { c = undefined_thing; }",
                "toy"
            ),
            Err(ShaderError::Parse(_))
        ));
        assert!(shadertoy_to_wgsl("void notMain() {}", "toy").is_err());
        assert!(
            shadertoy_to_wgsl(
                "void mainImage(out vec4 c, in vec2 f) { c = vec4(1.0); }",
                "1bad"
            )
            .is_err()
        );
    }
}
