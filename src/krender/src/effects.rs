//! 现成的后处理效果：three.js `addons/tsl/display/` 的那一套。
//!
//! 每个函数返回一个配好参数的 [`PostEffect`]，放进
//! [`PostStack`](crate::PostStack) 就能用；之后按名字改参数：
//!
//! ```ignore
//! ctx.post_effects.push(effects::chromatic_aberration());
//! ctx.post_effects.get_mut("chromatic_aberration").unwrap().set("strength", 1.5);
//! ```
//!
//! 参数名在各函数的文档里列着。颜色参数是线性的。
//!
//! 引擎自己的 TAA / SMAA 也在这里（[`taa`]、[`smaa`]），通过
//! [`AntiAlias`](crate::AntiAlias) 开关，一般不必手动放进链里。

use crate::postfx::{PassOutput, PostEffect, PostInputs, PostStage};
use kmath::{Vec2, Vec3};
use ktexture::Texture;

const SIMPLE: &str = include_str!("effects/simple.wgsl");
const DOF: &str = include_str!("effects/dof.wgsl");
const MOTION: &str = include_str!("effects/motion.wgsl");
const STYLIZE: &str = include_str!("effects/stylize.wgsl");
const LENS: &str = include_str!("effects/lens.wgsl");
const SSR: &str = include_str!("effects/ssr.wgsl");
const SSGI: &str = include_str!("effects/ssgi.wgsl");

/// TAA。由 [`AntiAlias::Taa`](crate::AntiAlias::Taa) 打开，排在 HDR 段的最后。
pub fn taa() -> PostEffect {
    PostEffect::new("taa", PostStage::Hdr, include_str!("effects/taa.wgsl"))
        .inputs(PostInputs::VELOCITY | PostInputs::JITTER)
        .history(PassOutput::Out)
        .pass("resolve", PassOutput::Out)
}

/// SMAA 风格的形态学抗锯齿。由 [`AntiAlias::Smaa`](crate::AntiAlias::Smaa)
/// 打开，排在 LDR 段的最后。和原版的差别见 `effects/smaa.wgsl`。
pub fn smaa() -> PostEffect {
    PostEffect::new("smaa", PostStage::Ldr, include_str!("effects/smaa.wgsl"))
        .scratch(1.0)
        .scratch(1.0)
        .pass("edges", PassOutput::Scratch(0))
        .pass("weights", PassOutput::Scratch(1))
        .pass("blend", PassOutput::Out)
}

/// 色差。参数：`strength`（0..3）、`center`（-1..1）、`scale`（0.5..2）。
pub fn chromatic_aberration() -> PostEffect {
    PostEffect::new("chromatic_aberration", PostStage::Ldr, SIMPLE)
        .param("strength", 1.5)
        .param("center", Vec2::ZERO)
        .param("scale", 1.2)
        .pass("chromatic_aberration", PassOutput::Out)
}

/// Sobel 边缘检测，输出灰度。无参数。
pub fn sobel() -> PostEffect {
    PostEffect::new("sobel", PostStage::Ldr, SIMPLE).pass("sobel", PassOutput::Out)
}

/// 3D LUT 调色。`strip` 是 `N² × N` 的条带（见 [`ktexture::lut`]）。
/// 参数：`size`（N）、`intensity`（0..1）。
pub fn lut(strip: Texture, size: u32, intensity: f32) -> PostEffect {
    PostEffect::new("lut", PostStage::Ldr, SIMPLE)
        .param("size", size as f32)
        .param("intensity", intensity)
        .texture(0, strip)
        .pass("lut", PassOutput::Out)
}

/// 残影：旧画面按 `damp` 衰减后和新画面取最大值。参数：`damp`（0.25..1）。
pub fn afterimage(damp: f32) -> PostEffect {
    PostEffect::new("afterimage", PostStage::Hdr, SIMPLE)
        .param("damp", damp)
        .history(PassOutput::Out)
        .pass("afterimage", PassOutput::Out)
}

/// 径向模糊。参数：`center`（UV）、`settings` = (weight, decay, count, exposure)。
pub fn radial_blur() -> PostEffect {
    PostEffect::new("radial_blur", PostStage::Hdr, SIMPLE)
        .param("center", Vec2::splat(0.5))
        .param("settings", [0.9, 0.95, 32.0, 5.0])
        .pass("radial_blur", PassOutput::Out)
}

/// 网点。参数：`center`（UV）、`settings` = (angle, scale)。
pub fn dot_screen() -> PostEffect {
    PostEffect::new("dot_screen", PostStage::Ldr, SIMPLE)
        .param("center", Vec2::splat(0.5))
        .param("settings", [1.57, 0.3])
        .pass("dot_screen", PassOutput::Out)
}

/// RGB 错位。参数：`settings` = (amount, angle)。
pub fn rgb_shift() -> PostEffect {
    PostEffect::new("rgb_shift", PostStage::Ldr, SIMPLE)
        .param("settings", [0.005, 0.0])
        .pass("rgb_shift", PassOutput::Out)
}

/// 暗角。参数：`settings` = (inner, outer, strength)，都按 UV 到中心的距离。
pub fn vignette() -> PostEffect {
    PostEffect::new("vignette", PostStage::Ldr, SIMPLE)
        .param("settings", [0.3, 0.75, 0.8])
        .pass("vignette", PassOutput::Out)
}

/// 帧差着色：静止的地方变灰，动起来的地方保持彩色。参数：`amplify`。
pub fn frame_difference(amplify: f32) -> PostEffect {
    PostEffect::new("frame_difference", PostStage::Hdr, SIMPLE)
        .param("amplify", amplify)
        .scratch(1.0)
        .history(PassOutput::Scratch(0))
        .pass("difference_store", PassOutput::Scratch(0))
        .pass("difference", PassOutput::Out)
}

/// 转场：从主相机过渡到离屏视图 `view` 的画面。
/// 参数：`settings` = (progress, threshold, use_texture)。`pattern` 是过渡用的灰度图。
pub fn transition(view: u8, pattern: Option<Texture>) -> PostEffect {
    let mut effect = PostEffect::new("transition", PostStage::Hdr, SIMPLE)
        .param(
            "settings",
            [0.0, 0.1, if pattern.is_some() { 1.0 } else { 0.0 }],
        )
        .view(0, view)
        .pass("transition", PassOutput::Out);
    effect.set_texture(1, pattern);
    effect
}

/// 调试视图：把某个缓冲原样显示出来。参数：`settings` = (mode, depth_range)。
///
/// | mode | 显示 |
/// |---|---|
/// | 0 | 画面本身 |
/// | 1 | 深度（除以 depth_range） |
/// | 2 | 世界法线 |
/// | 3 | 运动向量 |
/// | 4 | SSAO |
/// | 5 | 后处理遮罩 |
/// | 6 | 基础色 |
/// | 7 | 粗糙度（红）/ 金属度（绿） |
/// | 8 | 接触阴影 |
pub fn debug_view() -> PostEffect {
    PostEffect::new("debug_view", PostStage::Ldr, SIMPLE)
        .param("settings", [0.0, 50.0])
        .inputs(PostInputs::NORMAL | PostInputs::VELOCITY | PostInputs::MATERIAL | PostInputs::MASK)
        .pass("debug_view", PassOutput::Out)
}

/// 简单景深：整张图盒式模糊，按离对焦面的距离插值。
/// 参数：`focus` = (focus_distance, min_distance, max_distance)、`blur` = (size, spread)。
pub fn dof_basic() -> PostEffect {
    PostEffect::new("dof_basic", PostStage::Hdr, DOF)
        .param("focus", [5.0, 1.0, 3.0])
        .param("blur", [2.0, 4.0])
        .scratch(1.0)
        .pass("box_blur", PassOutput::Scratch(0))
        .pass("basic_composite", PassOutput::Out)
}

/// 散景景深。参数：`settings` = (focus_distance, focus_range, bokeh_radius_px)。
pub fn bokeh_dof() -> PostEffect {
    PostEffect::new("bokeh_dof", PostStage::Hdr, DOF)
        .param("settings", [10.0, 20.0, 8.0])
        .scratch(0.5)
        .scratch(0.5)
        .scratch(0.5)
        .pass("bokeh_prepare", PassOutput::Scratch(0))
        .pass("bokeh_gather", PassOutput::Scratch(1))
        .pass("bokeh_tent", PassOutput::Scratch(2))
        .pass("bokeh_composite", PassOutput::Out)
}

/// 运动模糊。参数：`settings` = (amount, samples)。
pub fn motion_blur() -> PostEffect {
    PostEffect::new("motion_blur", PostStage::Hdr, MOTION)
        .param("settings", [1.0, 16.0])
        .inputs(PostInputs::VELOCITY)
        .pass("motion_blur", PassOutput::Out)
}

/// 体积光：沿视线查阴影图累积，和 three.js 的 GodraysNode 同一个公式。
/// 需要一盏投影的方向光。
///
/// 参数：`march` = (density, max_density, steps, distance_attenuation)、
/// `anchor` = (光源位置 xyz, 作用范围)、`blend` = (混合色 rgb, blur)。
pub fn godrays() -> PostEffect {
    PostEffect::new("godrays", PostStage::Hdr, MOTION)
        .param("march", [0.7, 0.5, 60.0, 2.0])
        .param("anchor", [0.0, 50.0, 0.0, 500.0])
        .param("blend", [1.0, 1.0, 1.0, 1.0])
        .scratch(0.5)
        .scratch(0.5)
        .pass("godrays_march", PassOutput::Scratch(0))
        .pass("godrays_blur", PassOutput::Scratch(1))
        .pass("godrays_composite", PassOutput::Out)
}

/// 描边（遮罩通道 `channel` 里的物体）。
///
/// 参数：`visible_color`、`hidden_color`、`settings` = (thickness, strength, glow, pulse_period)、
/// `channel`。
pub fn outline(channel: u8) -> PostEffect {
    PostEffect::new("outline", PostStage::Ldr, STYLIZE)
        .param("visible_color", Vec3::ONE)
        .param("hidden_color", Vec3::new(0.19, 0.09, 0.09))
        .param("settings", [1.0, 3.0, 0.0, 0.0])
        .param("channel", f32::from(channel.min(3)))
        .inputs(PostInputs::MASK)
        .scratch(1.0)
        .scratch(0.5)
        .pass("outline_edges", PassOutput::Scratch(0))
        .pass("outline_glow", PassOutput::Scratch(1))
        .pass("outline_composite", PassOutput::Out)
}

/// 像素化 + 描边。参数：`settings` = (pixel_size, normal_edge, depth_edge)。
pub fn pixelation() -> PostEffect {
    PostEffect::new("pixelation", PostStage::Ldr, STYLIZE)
        .param("settings", [6.0, 0.3, 0.4])
        .inputs(PostInputs::NORMAL)
        .pass("pixelate", PassOutput::Out)
}

/// 复古：降分辨率、色阶、Bayer 抖动、扫描线、暗角、桶形畸变、色彩渗漏。
///
/// 参数：`a` = (pixel_size, color_levels, dither, scanlines)、
/// `b` = (scanline_density, vignette, curvature, bleeding)。
pub fn retro() -> PostEffect {
    PostEffect::new("retro", PostStage::Ldr, STYLIZE)
        .param("a", [3.0, 16.0, 1.0, 0.25])
        .param("b", [240.0, 0.6, 0.08, 0.5])
        .pass("retro", PassOutput::Out)
}

/// 宽银幕光条。参数：`settings` = (threshold, intensity, samples, step_px)、`tint`。
pub fn anamorphic() -> PostEffect {
    PostEffect::new("anamorphic", PostStage::Hdr, LENS)
        .param("settings", [0.3, 5.0, 80.0, 4.0])
        .param("tint", Vec3::new(0.48, 0.54, 1.0))
        .scratch(0.25)
        .scratch(0.25)
        .pass("anamorphic_bright", PassOutput::Scratch(0))
        .pass("anamorphic_streak", PassOutput::Scratch(1))
        .pass("anamorphic_composite", PassOutput::Out)
}

/// 镜头光晕（鬼影 + 光环）。
///
/// 参数：`ghosts` = (threshold, spacing, attenuation, count)、
/// `halo` = (width, strength, dispersion, intensity)。
pub fn lensflare() -> PostEffect {
    PostEffect::new("lensflare", PostStage::Hdr, LENS)
        .param("ghosts", [1.0, 0.25, 25.0, 6.0])
        .param("halo", [0.45, 0.3, 1.5, 1.0])
        .scratch(0.5)
        .scratch(0.5)
        .scratch(0.5)
        .pass("flare_bright", PassOutput::Scratch(0))
        .pass("flare_ghosts", PassOutput::Scratch(1))
        .pass("flare_blur", PassOutput::Scratch(2))
        .pass("flare_composite", PassOutput::Out)
}

/// 屏幕空间反射。`denoise` 为真时射线按粗糙度随机化、再做时间性累积
/// （顺带要运动向量和抖动）。
///
/// 参数：`trace` = (max_distance, thickness, steps, blur)、
/// `look` = (randomness, compare_split, intensity, temporal)。
/// `compare_split` ≥ 0 时画面按这条竖线分屏：左边原始，右边处理后。
pub fn ssr(denoise: bool) -> PostEffect {
    let mut effect = PostEffect::new("ssr", PostStage::Hdr, SSR)
        .param("trace", [4.0, 0.15, 64.0, 1.0])
        .param(
            "look",
            [
                if denoise { 1.0 } else { 0.0 },
                -1.0,
                1.0,
                if denoise { 1.0 } else { 0.0 },
            ],
        )
        .inputs(PostInputs::NORMAL | PostInputs::MATERIAL)
        .scratch(0.5)
        .scratch(0.5)
        .pass("ssr_trace", PassOutput::Scratch(0))
        .pass("ssr_blur", PassOutput::Scratch(1));
    if denoise {
        effect = effect
            .inputs(PostInputs::VELOCITY | PostInputs::JITTER)
            .scratch(0.5)
            .history(PassOutput::Scratch(2))
            .pass("ssr_temporal", PassOutput::Scratch(2));
    }
    effect.pass("ssr_composite", PassOutput::Out)
}

/// 屏幕空间全局光照 + AO。
///
/// 参数：`trace` = (slices, steps, radius, thickness)、
/// `look` = (gi_intensity, ao_intensity, temporal, output)。
/// `output`：0 = 合成，1 = 只看 GI，2 = 只看 AO。
pub fn ssgi() -> PostEffect {
    PostEffect::new("ssgi", PostStage::Hdr, SSGI)
        .param("trace", [2.0, 8.0, 2.0, 0.5])
        .param("look", [1.0, 1.0, 1.0, 0.0])
        .inputs(
            PostInputs::NORMAL | PostInputs::MATERIAL | PostInputs::VELOCITY | PostInputs::JITTER,
        )
        .scratch(0.5)
        .scratch(0.5)
        .scratch(0.5)
        .history(PassOutput::Scratch(1))
        .pass("ssgi_trace", PassOutput::Scratch(0))
        .pass("ssgi_temporal", PassOutput::Scratch(1))
        .pass("ssgi_denoise", PassOutput::Scratch(2))
        .pass("ssgi_composite", PassOutput::Out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<PostEffect> {
        vec![
            taa(),
            smaa(),
            chromatic_aberration(),
            sobel(),
            lut(Texture::white(), 2, 1.0),
            afterimage(0.9),
            radial_blur(),
            dot_screen(),
            rgb_shift(),
            vignette(),
            frame_difference(1000.0),
            transition(0, None),
            debug_view(),
            dof_basic(),
            bokeh_dof(),
            motion_blur(),
            godrays(),
            outline(0),
            pixelation(),
            retro(),
            anamorphic(),
            lensflare(),
            ssr(false),
            ssr(true),
            ssgi(),
        ]
    }

    #[test]
    fn every_stock_effect_passes_validation() {
        for effect in all() {
            if let Err(error) = kshader::Shader::from_wgsl(effect.full_source()) {
                panic!("「{}」编译失败：{error}", effect.name());
            }
        }
    }

    #[test]
    fn every_pass_entry_point_exists() {
        // 入口名写在字符串里，拼错了要到运行时 wgpu 才报。
        for effect in all() {
            let source = effect.full_source();
            for pass in &effect.debug_entries() {
                assert!(
                    source.contains(&format!("fn {pass}(")),
                    "「{}」的入口 {pass} 不存在",
                    effect.name()
                );
            }
        }
    }
}
