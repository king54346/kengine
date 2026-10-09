//! 水下后处理：相机沉到水面以下时，画面按视线距离吸收、染上水色、轻轻晃动。
//!
//! ```ignore
//! ctx.post_effects.push(underwater::effect());
//! // 每帧：
//! let water = ocean.sample(cam.x, cam.z);
//! underwater::update(ctx.post_effects, cam, &water, &UnderwaterLook::default(), (sun_dir, sun_brightness));
//! ```

use crate::ocean::WaterSample;
use kmath::Vec3;
use krender::{PassOutput, PostEffect, PostStack, PostStage};

const SHADER: &str = include_str!("underwater.wgsl");
/// 效果在后处理栈里的名字。
pub const NAME: &str = "kcomponents.underwater";

/// 水下的样子。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnderwaterLook {
    /// 水色（远处融进去的颜色）。
    pub color: Vec3,
    /// 每米吸收。
    pub absorption: Vec3,
    /// 雾浓度。
    pub fog: f32,
    /// 画面晃动强度。
    pub distortion: f32,
    /// 朝上看时水面透下来的光有多亮。
    pub light_from_above: f32,
    /// 光柱（太阳从水面射下来的一束束光）强度，0 = 不画。
    pub light_shafts: f32,
}

impl Default for UnderwaterLook {
    fn default() -> Self {
        Self {
            color: Vec3::new(0.02, 0.16, 0.2),
            absorption: Vec3::new(0.3, 0.06, 0.04),
            fog: 0.03,
            distortion: 1.0,
            light_from_above: 1.2,
            light_shafts: 1.0,
        }
    }
}

/// 建效果。HDR 阶段：色调映射之前做，吸收和雾在线性空间里才对。
pub fn effect() -> PostEffect {
    // 深度总是给的，不用声明输入。
    PostEffect::new(NAME, PostStage::Hdr, SHADER)
        .param("settings", [0.0, 0.03, 1.0, 0.0])
        .param("color", [0.02, 0.16, 0.2, 1.2])
        .param("absorb", [0.3, 0.06, 0.04, 0.0])
        .param("plane", [0.0, 1.0, 0.0, 0.0])
        .param("sun", [0.3, 0.8, 0.2, 3.0])
        .pass("underwater", PassOutput::Out)
}

/// 每帧更新：相机位置、它正下方（或正上方）的水面、太阳（方向、亮度）。
///
/// 相机离水面两米以内或在水下时启用：水线可能横穿画面，逐像素判断哪边是水。
pub fn update(
    stack: &mut PostStack,
    camera: Vec3,
    water: &WaterSample,
    look: &UnderwaterLook,
    sun: (Vec3, f32),
) {
    let Some(effect) = stack.get_mut(NAME) else {
        return;
    };
    let depth = water.height - camera.y;
    let enabled = depth > -2.0;
    effect.set(
        "settings",
        [
            if enabled { 1.0 } else { 0.0 },
            look.fog,
            look.distortion,
            depth.max(0.0),
        ],
    );
    effect.set(
        "color",
        [
            look.color.x,
            look.color.y,
            look.color.z,
            look.light_from_above,
        ],
    );
    effect.set(
        "absorb",
        [
            look.absorption.x,
            look.absorption.y,
            look.absorption.z,
            look.light_shafts,
        ],
    );
    effect.set("sun", [sun.0.x, sun.0.y, sun.0.z, sun.1]);
    // 水面在相机处的切平面：过 (x, 水面高, z)、法线是水面法线。
    let point = Vec3::new(camera.x, water.height, camera.z);
    let n = water.normal;
    effect.set("plane", [n.x, n.y, n.z, -n.dot(point)]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_effect_compiles() {
        // 拼上引擎的后处理前缀之后交给 naga 解析、校验。
        let source = super::effect().full_source();
        kshader::Shader::from_wgsl(source).expect("underwater.wgsl 编不过");
    }
}
