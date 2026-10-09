//! 草地下面那块地：起伏的高度、草地 / 泥地的分布、地面的顶点色。
//!
//! 照 three-stylized 的 `Terrain.ts` 移植，噪声逐式一致（同一个种子长出同一块地）：
//! 值噪声 → 四层 fbm → 域扭曲的 fbm。泥地是低频的大块 + 中频的边 + 高频的「喷溅」，
//! 草只长在泥地以外、而且草地本身也有疏密（`grass_coverage`）。

use kmath::{Vec2, Vec3};
use kmesh::{Mesh, Vertex};

/// 地形网格的格子不超过这么大（米）：地块放大时自动加细分，起伏不至于变成折线。
const MAX_CELL_SIZE: f32 = 0.56;
/// 格子中心的草覆盖低于这个值就整格不长草（采样面上没有这一格）。
const GRASS_CUTOFF: f32 = 0.42;

/// 地形参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainSettings {
    /// 宽（x）、深（z），米。地块以原点为中心。
    pub size: Vec2,
    /// 细分（每边至少这么多格）。
    pub segments: u32,
    pub seed: u32,
    /// 起伏程度 0..1（0 = 平地）。
    pub relief: f32,
    /// 草地底色（sRGB）。
    pub ground_color: Vec3,
}

impl Default for TerrainSettings {
    fn default() -> Self {
        Self {
            size: Vec2::splat(20.0),
            segments: 72,
            seed: 17,
            relief: 0.8,
            ground_color: super::hex(0x557d24),
        }
    }
}

fn hash(x: f64, z: f64, seed: f64) -> f64 {
    let value = (x * 127.1 + z * 311.7 + seed * 74.7).sin() * 43_758.545_312_3;
    value - value.floor()
}

fn value_noise(x: f64, z: f64, seed: f64) -> f64 {
    let (x0, z0) = (x.floor(), z.floor());
    let (tx, tz) = (x - x0, z - z0);
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sz = tz * tz * (3.0 - 2.0 * tz);
    let a = hash(x0, z0, seed);
    let b = hash(x0 + 1.0, z0, seed);
    let c = hash(x0, z0 + 1.0, seed);
    let d = hash(x0 + 1.0, z0 + 1.0, seed);
    let ab = a + (b - a) * sx;
    let cd = c + (d - c) * sx;
    ab + (cd - ab) * sz
}

fn fbm(x: f64, z: f64, seed: f64) -> f64 {
    let (mut total, mut amplitude, mut normalizer, mut frequency) = (0.0, 0.5, 0.0, 1.0);
    for octave in 0..4 {
        total +=
            value_noise(x * frequency, z * frequency, seed + octave as f64 * 101.0) * amplitude;
        normalizer += amplitude;
        frequency *= 2.03;
        amplitude *= 0.5;
    }
    total / normalizer
}

fn warped_fbm(x: f64, z: f64, seed: f64, warp: f64) -> f64 {
    let warp_x = fbm(x + 11.3, z + 2.7, seed + 79.0);
    let warp_z = fbm(x + 5.9, z + 17.1, seed + 151.0);
    fbm(x + (warp_x - 0.5) * warp, z + (warp_z - 0.5) * warp, seed)
}

fn smooth01(value: f64, edge0: f64, edge1: f64) -> f64 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 0 = 草地，1 = 裸露的泥地。
pub fn dirt_amount(x: f32, z: f32, seed: u32) -> f32 {
    let (x, z, seed) = (x as f64, z as f64, seed as f64);
    let macro_ = warped_fbm(x * 0.27, z * 0.27, seed, 1.7);
    let edge = warped_fbm(x * 0.73 + 13.1, z * 0.73 - 8.7, seed + 31.0, 0.85);
    let overspray = fbm(x * 1.55 - 4.2, z * 1.55 + 19.6, seed + 67.0);
    let paint = macro_ * 0.69 + edge * 0.23 + overspray * 0.08;
    smooth01(paint, 0.46, 0.7) as f32
}

/// 地面高度（米）。
pub fn height(x: f32, z: f32, seed: u32, relief: f32) -> f32 {
    let (x, z, seed) = (x as f64, z as f64, seed as f64);
    let broad = warped_fbm(x * 0.15, z * 0.15, seed + 211.0, 0.55) - 0.5;
    let detail = fbm(x * 0.62, z * 0.62, seed + 29.0) - 0.5;
    let amount = smooth01(relief as f64, 0.0, 1.0);
    ((broad * 3.0 + detail * 0.45) * amount) as f32
}

/// 草有多密（0..1）：泥地上没有，草地上也有疏有密。
pub fn grass_coverage(x: f32, z: f32, seed: u32) -> f32 {
    let variation = 0.62 + fbm(x as f64 * 0.46, z as f64 * 0.46, seed as f64 + 63.0) as f32 * 0.38;
    (1.0 - dirt_amount(x, z, seed)) * variation
}

fn segments(settings: &TerrainSettings) -> (u32, u32) {
    let s = settings.segments.max(2);
    (
        s.max((settings.size.x / MAX_CELL_SIZE).ceil() as u32),
        s.max((settings.size.y / MAX_CELL_SIZE).ceil() as u32),
    )
}

/// 地面网格：起伏 + 顶点色（草地的深浅 + 泥地）。
pub fn ground_mesh(settings: &TerrainSettings) -> Mesh {
    let (ws, ds) = segments(settings);
    let size = settings.size;
    let (seed, relief) = (settings.seed, settings.relief);
    let h = |x: f32, z: f32| height(x, z, seed, relief);
    let meadow = super::srgb(settings.ground_color);
    let meadow_shade = super::offset_lightness(meadow, -0.09);
    let (dirt, dirt_shade) = (
        super::srgb(super::hex(0x94744a)),
        super::srgb(super::hex(0x6f5435)),
    );
    let mut vertices = Vec::with_capacity(((ws + 1) * (ds + 1)) as usize);
    for row in 0..=ds {
        for column in 0..=ws {
            let x = -size.x * 0.5 + size.x * column as f32 / ws as f32;
            let z = -size.y * 0.5 + size.y * row as f32 / ds as f32;
            // 法线用高度函数的中心差分，不用三角形平均：边上的顶点也对。
            let e = 0.05;
            let normal = Vec3::new(
                h(x - e, z) - h(x + e, z),
                2.0 * e,
                h(x, z - e) - h(x, z + e),
            )
            .normalize();
            let variation = fbm(x as f64 * 0.55, z as f64 * 0.55, seed as f64 + 31.0) as f32;
            let grass = meadow_shade.lerp(meadow, variation);
            let soil = dirt_shade.lerp(
                dirt,
                fbm(x as f64 * 0.72, z as f64 * 0.72, seed as f64 + 47.0) as f32,
            );
            let color = grass.lerp(soil, dirt_amount(x, z, seed));
            let uv = [column as f32 / ws as f32, row as f32 / ds as f32];
            vertices.push(Vertex::new(Vec3::new(x, h(x, z), z), normal, uv).with_color(color));
        }
    }
    let mut indices = Vec::with_capacity((ws * ds * 6) as usize);
    let stride = ws + 1;
    for row in 0..ds {
        for column in 0..ws {
            let a = row * stride + column;
            let (b, c, d) = (a + 1, a + stride, a + stride + 1);
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    Mesh::new(vertices, indices)
}

/// 长草的那些三角形：草覆盖够的格子（按格子中心判断）切成两个三角形。
pub fn grass_triangles(settings: &TerrainSettings) -> Vec<[Vec3; 3]> {
    let (ws, ds) = segments(settings);
    let size = settings.size;
    let (cw, cd) = (size.x / ws as f32, size.y / ds as f32);
    let p = |x: f32, z: f32| Vec3::new(x, height(x, z, settings.seed, settings.relief), z);
    let mut triangles = Vec::new();
    for row in 0..ds {
        for column in 0..ws {
            let x0 = -size.x * 0.5 + column as f32 * cw;
            let z0 = -size.y * 0.5 + row as f32 * cd;
            let (x1, z1) = (x0 + cw, z0 + cd);
            if grass_coverage((x0 + x1) * 0.5, (z0 + z1) * 0.5, settings.seed) < GRASS_CUTOFF {
                continue;
            }
            triangles.push([p(x0, z0), p(x1, z1), p(x1, z0)]);
            triangles.push([p(x0, z0), p(x0, z1), p(x1, z1)]);
        }
    }
    triangles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_grows_the_same_land() {
        assert_eq!(height(3.2, -1.7, 17, 0.8), height(3.2, -1.7, 17, 0.8));
        assert_ne!(height(3.2, -1.7, 17, 0.8), height(3.2, -1.7, 18, 0.8));
        assert_eq!(height(3.2, -1.7, 17, 0.0), 0.0, "起伏 0 是平地");
    }

    #[test]
    fn grass_avoids_the_dirt_patches() {
        let settings = TerrainSettings::default();
        let triangles = grass_triangles(&settings);
        assert!(!triangles.is_empty());
        let (ws, ds) = segments(&settings);
        let all = (ws * ds * 2) as usize;
        // 默认那块地（种子 17）一成左右是泥地，和原版截图里那几块差不多：有，但不多。
        assert!(
            triangles.len() < all * 97 / 100 && triangles.len() > all / 2,
            "{} / {all}",
            triangles.len()
        );
        for t in &triangles {
            let center = (t[0] + t[1] + t[2]) / 3.0;
            assert!(dirt_amount(center.x, center.z, settings.seed) < 0.9);
        }
    }
}
