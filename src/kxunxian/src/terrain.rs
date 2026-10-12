//! 地表分层混合：把 `layer.xml` 的各层贴图和 `bits_alpha.byte` 的权重做成每个地形单元一份材质。
//!
//! 预混贴图（`preblend`）每米只有一个像素，近看是一片糊的色块；游戏近处画的是各层贴图按权重
//! 逐像素混出来的地面（草、路、山石、河床……每层几米重复一次）。这里照做，远处再淡回预混贴图。
//! 混合的规则和权重的排布见 [`Blend`](crate::scene::Blend)，着色器见 `terrain.wgsl`。

use std::collections::{BTreeMap, BTreeSet};

use kasset::Resource;
use kmaterial::Material;
use kmath::Vec4;
use ktexture::{Sampler, Texture, TextureFormat};

use crate::character::Library;
use crate::scene::{SceneData, UNIT_SIZE, Unit};

const SHADER: &str = include_str!("terrain.wgsl");
/// 纹理数组每层的边长：各层贴图有 256 也有 512 的，统一缩到 256（草地 6 米一重复时每米还有 40 多像素）。
const LAYER_SIZE: usize = 256;
/// 一个单元最多混几层（底层之外）：3 张 RGBA 权重图。第 4 张自定义贴图槽位给预混贴图。
const CHANNELS: usize = 12;
/// 离镜头多远开始淡回预混贴图、多远完全换掉（米）。
const FADE: (f32, f32) = (60.0, 140.0);

impl Library {
    /// 每个地形单元一份地表材质，顺序同 `scene.units`。没有分层数据的单元退回只贴预混贴图。
    pub fn terrain_materials(&mut self, scene: &SceneData) -> Vec<Material> {
        // 整张地图用到的层，每层一张贴图，叠成一个纹理数组大家共用。
        let used: BTreeSet<usize> = scene
            .units
            .iter()
            .filter_map(|u| u.blend.as_ref())
            .flat_map(|b| b.layers.iter().copied())
            .collect();
        let mut slots = BTreeMap::new();
        let mut pixels = Vec::new();
        for layer in used {
            let Some(def) = scene.layers.get(layer).and_then(Option::as_ref) else {
                continue;
            };
            let Some(rgba) = self.layer_pixels(&def.texture) else {
                klog::warn!(
                    "地表层 {layer}（{}）的贴图 {} 读不到",
                    def.alias,
                    def.texture
                );
                continue;
            };
            slots.insert(layer, (slots.len() as f32, def.scale));
            pixels.extend_from_slice(&rgba);
        }
        let array = (!slots.is_empty()).then(|| {
            Resource::new_ok(
                format!("{}/terrain_layers", scene.name),
                Texture::array(
                    LAYER_SIZE as u32,
                    LAYER_SIZE as u32,
                    slots.len() as u32,
                    pixels,
                )
                .with_format(TextureFormat::Srgb),
            )
        });
        let shader = Resource::new_ok("kxunxian/terrain.wgsl", kshader::Shader::snippet(SHADER));
        scene
            .units
            .iter()
            .map(|unit| match &array {
                Some(array) => blended(unit, array, &slots, &shader),
                None => plain(unit),
            })
            .collect()
    }

    /// 读一层地表贴图，缩放成 `LAYER_SIZE` 见方的 RGBA。
    fn layer_pixels(&self, raw: &str) -> Option<Vec<u8>> {
        let (_, bytes) = self.read(raw).ok()?;
        let texture = Texture::from_encoded(&bytes).ok()?;
        Some(resample(
            texture.data(),
            texture.width() as usize,
            texture.height() as usize,
        ))
    }
}

/// 缩放到 `LAYER_SIZE` 见方：缩小按块平均（整数倍），放大或不整除时取最近点。
fn resample(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; LAYER_SIZE * LAYER_SIZE * 4];
    let (fx, fy) = (width / LAYER_SIZE, height / LAYER_SIZE);
    let boxed =
        fx >= 1 && fy >= 1 && width.is_multiple_of(LAYER_SIZE) && height.is_multiple_of(LAYER_SIZE);
    for y in 0..LAYER_SIZE {
        for x in 0..LAYER_SIZE {
            let target = &mut out[(y * LAYER_SIZE + x) * 4..][..4];
            if boxed {
                let mut sum = [0u32; 4];
                for dy in 0..fy {
                    for dx in 0..fx {
                        let at = ((y * fy + dy) * width + x * fx + dx) * 4;
                        for c in 0..4 {
                            sum[c] += data[at + c] as u32;
                        }
                    }
                }
                for c in 0..4 {
                    target[c] = (sum[c] / (fx * fy) as u32) as u8;
                }
            } else {
                let at = ((y * height / LAYER_SIZE) * width + x * width / LAYER_SIZE) * 4;
                target.copy_from_slice(&data[at..at + 4]);
            }
        }
    }
    out
}

/// 「层号 × 100 + `Scale`」：着色器里一个数就能拆出两样。
fn code((slot, scale): (f32, f32)) -> f32 {
    slot * 100.0 + scale.min(99.0)
}

/// 只贴预混贴图（没有分层数据时）。
fn plain(unit: &Unit) -> Material {
    let mut material = Material::default().with_roughness(1.0).with_metallic(0.0);
    if let Some(texture) = unit.preblend.clone() {
        material = material.with_base_color_texture(texture);
    }
    material
}

fn blended(
    unit: &Unit,
    array: &Resource<Texture>,
    slots: &BTreeMap<usize, (f32, f32)>,
    shader: &Resource<kshader::Shader>,
) -> Material {
    let Some(blend) = &unit.blend else {
        return plain(unit);
    };
    // 底层：第一个有贴图的层。它之前的层（如果有）没贴图可画，跳过。
    let Some(base) = blend.layers.iter().position(|l| slots.contains_key(l)) else {
        return plain(unit);
    };
    // 往上的层：有贴图、权重不全是 0 的才占通道。
    let channels: Vec<usize> = (base + 1..blend.layers.len())
        .filter(|&k| slots.contains_key(&blend.layers[k]))
        .filter(|&k| blend.weights[k].iter().any(|&w| w > 0))
        .collect();
    if channels.len() > CHANNELS {
        klog::warn!(
            "单元 ({},{}) 有 {} 层要混，只画得下前 {CHANNELS} 层",
            unit.x,
            unit.z,
            channels.len()
        );
    }
    let channels = &channels[..channels.len().min(CHANNELS)];

    // 权重打包成 4 张 RGBA：像素排布和预混贴图一样（行 = 本地z，列 = 255 − 本地x），
    // 这样直接用地形网格的 uv 就能采到。
    let size = UNIT_SIZE;
    let mut packed = vec![vec![0u8; size * size * 4]; CHANNELS / 4];
    for (channel, &k) in channels.iter().enumerate() {
        let (image, component) = (channel / 4, channel % 4);
        for lx in 0..size {
            for lz in 0..size {
                let pixel = lz * size + (size - 1 - lx);
                packed[image][pixel * 4 + component] = blend.weights[k][lx * size + lz];
            }
        }
    }

    // 基础色贴图是一张白图：着色器里 `base_color` 于是正好是顶点色（地表明暗）。
    // 它的采样器就是所有贴图共用的那个（重复平铺；地面多是掠射角，开各向异性）。
    let white = Texture::white().with_sampler(Sampler {
        anisotropy: 8,
        ..Sampler::default()
    });
    let mut material = Material::default()
        .with_roughness(1.0)
        .with_metallic(0.0)
        .with_shader(shader.clone())
        .with_texture_array(array.clone())
        .with_base_color_texture(Resource::new_ok("kxunxian/terrain_white", white));
    if let Some(preblend) = unit.preblend.clone() {
        material = material.with_custom_texture(CHANNELS / 4, preblend);
    }
    for (slot, pixels) in packed.into_iter().enumerate() {
        let texture =
            Texture::new(size as u32, size as u32, pixels).with_format(TextureFormat::Linear);
        material = material.with_custom_texture(
            slot,
            Resource::new_ok(
                format!("terrain_weights_{}_{}_{slot}", unit.x, unit.z),
                texture,
            ),
        );
    }
    let mut codes = [-1.0f32; CHANNELS];
    for (channel, &k) in channels.iter().enumerate() {
        codes[channel] = code(slots[&blend.layers[k]]);
    }
    for slot in 0..CHANNELS / 4 {
        let c = &codes[slot * 4..slot * 4 + 4];
        material = material.with_param(slot, Vec4::new(c[0], c[1], c[2], c[3]));
    }
    material.with_param(
        4,
        Vec4::new(code(slots[&blend.layers[base]]), FADE.0, FADE.1, 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_box_filters_down_and_keeps_size() {
        let big = vec![200u8; 512 * 512 * 4];
        let out = resample(&big, 512, 512);
        assert_eq!(out.len(), LAYER_SIZE * LAYER_SIZE * 4);
        assert!(out.iter().all(|&v| v == 200));
        let small = vec![7u8; 128 * 128 * 4];
        assert!(resample(&small, 128, 128).iter().all(|&v| v == 7));
    }

    #[test]
    fn code_round_trips_slot_and_scale() {
        let c = code((3.0, 6.0));
        let layer = (c / 100.0).floor();
        assert_eq!(layer, 3.0);
        assert_eq!(c - layer * 100.0, 6.0);
    }
}
