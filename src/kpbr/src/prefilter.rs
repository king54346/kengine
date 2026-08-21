//! 镜面预滤波：把环境图按粗糙度卷积成一条 mip 链。
//!
//! # 为什么要预滤波
//!
//! 一个粗糙表面反射的不是某一个方向，而是**一整片锥形区域**里的入射光。
//! 实时地对每个像素积分几百个方向是不可能的，所以离线把结果算好：
//! 第 0 级是原图（完全光滑），越往下越模糊（越粗糙）。
//!
//! 着色时按 `roughness` 选 mip 级，硬件的三线性过滤顺便把两级之间插好。
//!
//! # 分裂求和近似
//!
//! 完整的镜面 IBL 积分含 BRDF 项，没法只靠一张图表示。业界做法是
//! 把积分拆成两半（Karis 2013，*Real Shading in Unreal Engine 4*）：
//! **预滤波的环境图** × **一张与环境无关的 BRDF 查找表**。
//! 后者引擎里已经有了（见 [`brdf`](crate::brdf)）。
//!
//! # 近似的地方
//!
//! 预滤波时假设**观察方向 = 法线 = 反射方向**。这在掠射角下不成立，
//! 表现为掠射角的反射会偏模糊。这是分裂求和法公认的代价，
//! UE4 到现在也是这么做的。

use crate::hdr::HdrImage;
use kmath::Vec3;

/// 一级预滤波结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PrefilteredLevel {
    /// 这一级对应的粗糙度，0 是完全光滑。
    pub roughness: f32,
    /// 宽。
    pub width: usize,
    /// 高。
    pub height: usize,
    /// 行主序，每像素三个 `f32`。等距柱状投影，和源图同一套坐标。
    pub pixels: Vec<f32>,
}

impl PrefilteredLevel {
    /// 取一个像素。
    pub fn pixel(&self, x: usize, y: usize) -> Vec3 {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let index = (y * self.width + x) * 3;
        Vec3::new(
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
        )
    }
}

/// 预滤波的参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrefilterSettings {
    /// 第 0 级的宽度（高度是它的一半，等距柱状投影的比例）。
    pub base_width: usize,
    /// 生成几级。
    pub levels: usize,
    /// 每个像素采样几次。
    ///
    /// 太少的话粗糙的那几级会有明显的噪点——重要性采样的方差在
    /// 高粗糙度下最大，而那正是需要最模糊的地方。
    pub samples: u32,
}

impl Default for PrefilterSettings {
    fn default() -> Self {
        Self {
            // 256×128 对镜面反射够用：更高的分辨率只在完全光滑的
            // 镜面上看得出来，而那种表面通常该用反射探针或屏幕空间反射。
            base_width: 256,
            levels: 5,
            samples: 128,
        }
    }
}

/// 生成预滤波的 mip 链。
///
/// 返回 `levels` 级，第 0 级粗糙度为 0（原图降采样），最后一级为 1。
///
/// 级数会被夹住，保证每一级都**正好是上一级的一半**——GPU 的 mip 链
/// 是这么定义的。级数要得太多时后面几级会被 `max(8)` 夹成同样大小，
/// 那样的数组传不进纹理的 mip 链（尺寸对不上，wgpu 直接报错）。
pub fn prefilter(image: &HdrImage, settings: PrefilterSettings) -> Vec<PrefilteredLevel> {
    let levels = settings.levels.max(1).min(max_levels(settings.base_width));
    let mut out = Vec::with_capacity(levels);

    for level in 0..levels {
        // 每级尺寸严格减半。级数已经在上面夹过，这里不会低于 8。
        let width = settings.base_width >> level;
        let height = width / 2;
        let roughness = if levels == 1 {
            0.0
        } else {
            level as f32 / (levels - 1) as f32
        };

        let mut pixels = vec![0.0f32; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let normal = direction_of(x, y, width, height);
                let color = if roughness <= 0.0 {
                    // 粗糙度为 0 时卷积核退化成一个方向，直接采样。
                    // 走通用路径的话，重要性采样在 a=0 时会除以零。
                    image.sample_direction(normal)
                } else {
                    convolve(image, normal, roughness, settings.samples)
                };
                let index = (y * width + x) * 3;
                pixels[index] = color.x;
                pixels[index + 1] = color.y;
                pixels[index + 2] = color.z;
            }
        }

        out.push(PrefilteredLevel {
            roughness,
            width,
            height,
            pixels,
        });
    }
    out
}

/// 给定基础宽度，最多能生成几级严格减半的 mip。
///
/// 下限取 8 像素宽：再小的话等距柱状投影的极点区域会退化成
/// 一两个像素，反射会出现明显的横条。
pub fn max_levels(base_width: usize) -> usize {
    let mut width = base_width.max(8);
    let mut count = 1;
    while width / 2 >= 8 {
        width /= 2;
        count += 1;
    }
    count
}

/// 等距柱状投影里某个像素对应的方向。
fn direction_of(x: usize, y: usize, width: usize, height: usize) -> Vec3 {
    let u = (x as f32 + 0.5) / width as f32;
    let v = (y as f32 + 0.5) / height as f32;
    let phi = u * std::f32::consts::TAU - std::f32::consts::PI;
    let theta = v * std::f32::consts::PI;
    let (sin_theta, cos_theta) = theta.sin_cos();
    let (sin_phi, cos_phi) = phi.sin_cos();
    Vec3::new(sin_theta * cos_phi, cos_theta, sin_theta * sin_phi)
}

/// 对一个方向做 GGX 重要性采样的卷积。
fn convolve(image: &HdrImage, normal: Vec3, roughness: f32, samples: u32) -> Vec3 {
    let (tangent, bitangent) = orthonormal_basis(normal);
    let alpha = (roughness * roughness).max(1e-4);

    let mut sum = Vec3::ZERO;
    let mut weight = 0.0f32;

    for i in 0..samples {
        let xi = hammersley(i, samples);
        let half = importance_sample_ggx(xi, alpha, normal, tangent, bitangent);
        // 假设观察方向 = 法线，于是反射方向就是关于半程向量的镜像。
        let light = (2.0 * normal.dot(half) * half - normal).normalize_or(normal);

        let n_dot_l = normal.dot(light);
        if n_dot_l <= 0.0 {
            continue;
        }
        // 按 n·l 加权：背向法线的采样对结果没有贡献，
        // 不加权的话粗糙表面的反射会整体偏亮。
        sum += image.sample_direction(light) * n_dot_l;
        weight += n_dot_l;
    }

    if weight > 0.0 {
        sum / weight
    } else {
        image.sample_direction(normal)
    }
}

/// 低差异序列。比随机数收敛快得多——同样的采样数下噪点明显更少。
fn hammersley(index: u32, count: u32) -> (f32, f32) {
    // 位反转产生 Van der Corput 序列。
    let mut bits = index;
    bits = bits.rotate_right(16);
    bits = ((bits & 0x5555_5555) << 1) | ((bits & 0xAAAA_AAAA) >> 1);
    bits = ((bits & 0x3333_3333) << 2) | ((bits & 0xCCCC_CCCC) >> 2);
    bits = ((bits & 0x0F0F_0F0F) << 4) | ((bits & 0xF0F0_F0F0) >> 4);
    bits = ((bits & 0x00FF_00FF) << 8) | ((bits & 0xFF00_FF00) >> 8);
    (
        index as f32 / count.max(1) as f32,
        bits as f32 * 2.328_306_4e-10,
    )
}

/// 按 GGX 分布采一个半程向量。
fn importance_sample_ggx(
    xi: (f32, f32),
    alpha: f32,
    normal: Vec3,
    tangent: Vec3,
    bitangent: Vec3,
) -> Vec3 {
    let phi = std::f32::consts::TAU * xi.0;
    let cos_theta = ((1.0 - xi.1) / (1.0 + (alpha * alpha - 1.0) * xi.1))
        .max(0.0)
        .sqrt();
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let (sin_phi, cos_phi) = phi.sin_cos();

    let local = Vec3::new(sin_theta * cos_phi, sin_theta * sin_phi, cos_theta);
    (tangent * local.x + bitangent * local.y + normal * local.z).normalize_or(normal)
}

/// 给一个法线配一组正交基。
fn orthonormal_basis(normal: Vec3) -> (Vec3, Vec3) {
    // 用一个不与法线共线的参考向量。共线的话叉乘得到零向量，
    // 归一化之后是 NaN，整个采样全废。
    let up = if normal.y.abs() < 0.999 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tangent = up.cross(normal).normalize_or(Vec3::X);
    let bitangent = normal.cross(tangent);
    (tangent, bitangent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张上半亮、下半暗的 HDR 图。
    fn split_sky(width: usize, height: usize) -> HdrImage {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n");
        bytes.extend_from_slice(format!("-Y {height} +X {width}\n").as_bytes());
        for row in 0..height {
            // 指数 128 → 尾数/256。上半 200/256，下半 16/256。
            let value = if row < height / 2 { 200u8 } else { 16u8 };
            for _ in 0..width {
                bytes.extend_from_slice(&[value, value, value, 128]);
            }
        }
        HdrImage::decode(&bytes).expect("测试图该能解码")
    }

    /// 一张只有一个亮点的图，用来看模糊有没有扩散开。
    fn single_bright_spot(width: usize, height: usize) -> HdrImage {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n");
        bytes.extend_from_slice(format!("-Y {height} +X {width}\n").as_bytes());
        for row in 0..height {
            for col in 0..width {
                let bright = row == height / 2 && col == width / 2;
                let pixel = if bright {
                    [255u8, 255, 255, 140]
                } else {
                    [0, 0, 0, 0]
                };
                bytes.extend_from_slice(&pixel);
            }
        }
        HdrImage::decode(&bytes).unwrap()
    }

    fn small() -> PrefilterSettings {
        PrefilterSettings {
            // 64 才够生成四级严格减半的 mip（64→32→16→8）。
            // 用 32 的话只能出三级，级数会被夹掉一级。
            base_width: 64,
            levels: 4,
            samples: 32,
        }
    }

    #[test]
    fn the_chain_has_the_requested_number_of_levels() {
        let levels = prefilter(&split_sky(64, 32), small());
        assert_eq!(levels.len(), 4);
    }

    #[test]
    fn roughness_goes_from_zero_to_one() {
        let levels = prefilter(&split_sky(64, 32), small());
        assert_eq!(levels[0].roughness, 0.0);
        assert_eq!(levels[levels.len() - 1].roughness, 1.0);
        for pair in levels.windows(2) {
            assert!(pair[1].roughness > pair[0].roughness);
        }
    }

    #[test]
    fn each_level_is_half_the_size() {
        let levels = prefilter(
            &split_sky(64, 32),
            PrefilterSettings {
                base_width: 64,
                levels: 3,
                samples: 16,
            },
        );
        assert_eq!(levels[0].width, 64);
        assert_eq!(levels[1].width, 32);
        assert_eq!(levels[2].width, 16);
        // 等距柱状投影是 2:1。
        for level in &levels {
            assert_eq!(level.height, level.width / 2);
        }
    }

    #[test]
    fn levels_never_shrink_below_a_usable_size() {
        // 再小的话极点区域会退化成一两个像素，反射会出现明显的横条。
        let levels = prefilter(
            &split_sky(16, 8),
            PrefilterSettings {
                base_width: 16,
                levels: 8,
                samples: 8,
            },
        );
        for level in &levels {
            assert!(
                level.width >= 8 && level.height >= 4,
                "{}×{}",
                level.width,
                level.height
            );
        }
    }

    #[test]
    fn level_zero_is_the_source_image() {
        // 粗糙度为 0 时卷积核退化成一个方向。走通用路径的话，
        // 重要性采样在 a=0 时会除以零。
        let image = split_sky(64, 32);
        let levels = prefilter(&image, small());

        let up = direction_of(16, 2, levels[0].width, levels[0].height);
        let expected = image.sample_direction(up);
        let actual = levels[0].pixel(16, 2);
        assert!(
            (actual - expected).length() < 1e-4,
            "第 0 级该等于原图：{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn rougher_levels_are_blurrier() {
        // 这是预滤波的全部意义。不模糊的话，粗糙表面反射出来的
        // 会是一张清晰的镜像，看着像镜子而不是磨砂。
        let image = single_bright_spot(64, 32);
        let levels = prefilter(
            &image,
            PrefilterSettings {
                base_width: 32,
                levels: 4,
                samples: 64,
            },
        );

        // 用「最大值 / 平均值」衡量集中程度：越模糊越接近 1。
        let concentration = |level: &PrefilteredLevel| -> f32 {
            let mut max = 0.0f32;
            let mut sum = 0.0f32;
            for y in 0..level.height {
                for x in 0..level.width {
                    let v = level.pixel(x, y).x;
                    max = max.max(v);
                    sum += v;
                }
            }
            let average = sum / (level.width * level.height) as f32;
            if average > 0.0 { max / average } else { 0.0 }
        };

        let sharp = concentration(&levels[0]);
        let blurry = concentration(&levels[levels.len() - 1]);
        assert!(
            blurry < sharp,
            "最粗糙那级该更均匀：集中度 {sharp} → {blurry}"
        );
    }

    #[test]
    fn energy_is_roughly_preserved() {
        // 卷积不该整体变亮或变暗。不按 n·l 加权的话，
        // 粗糙表面的反射会明显偏亮。
        let image = split_sky(64, 32);
        let levels = prefilter(
            &image,
            PrefilterSettings {
                base_width: 32,
                levels: 4,
                samples: 128,
            },
        );

        let average = |level: &PrefilteredLevel| -> f32 {
            let sum: f32 = (0..level.height)
                .flat_map(|y| (0..level.width).map(move |x| (x, y)))
                .map(|(x, y)| level.pixel(x, y).x)
                .sum();
            sum / (level.width * level.height) as f32
        };

        let base = average(&levels[0]);
        let rough = average(&levels[levels.len() - 1]);
        assert!(
            (rough / base - 1.0).abs() < 0.35,
            "能量偏移太大：{base} → {rough}"
        );
    }

    #[test]
    fn no_nan_anywhere() {
        // 法线与参考向量共线时叉乘得零向量，归一化之后是 NaN，
        // 整个采样全废——而且画面上只是一片黑，不报错。
        let levels = prefilter(&split_sky(64, 32), small());
        for level in &levels {
            assert!(
                level.pixels.iter().all(|v| v.is_finite()),
                "粗糙度 {} 这一级有 NaN",
                level.roughness
            );
        }
    }

    #[test]
    fn the_poles_are_handled() {
        // 正上方和正下方是等距柱状投影的奇点，容易出 NaN。
        let levels = prefilter(&split_sky(64, 32), small());
        for level in &levels {
            let top = level.pixel(level.width / 2, 0);
            let bottom = level.pixel(level.width / 2, level.height - 1);
            assert!(top.is_finite() && bottom.is_finite());
        }
    }

    #[test]
    fn a_single_level_chain_is_sharp() {
        let levels = prefilter(
            &split_sky(32, 16),
            PrefilterSettings {
                base_width: 32,
                levels: 1,
                samples: 8,
            },
        );
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].roughness, 0.0);
    }

    #[test]
    fn hammersley_covers_the_unit_square() {
        // 低差异序列的意义：同样的采样数下噪点明显更少。
        // 全挤在一个角上的话，卷积会带明显的方向性偏差。
        let count = 64;
        let points: Vec<(f32, f32)> = (0..count).map(|i| hammersley(i, count)).collect();
        assert!(
            points
                .iter()
                .all(|(x, y)| (0.0..1.0).contains(x) && (0.0..1.0).contains(y))
        );

        // 四个象限都该有点。
        for (qx, qy) in [(false, false), (false, true), (true, false), (true, true)] {
            let found = points
                .iter()
                .any(|(x, y)| (*x >= 0.5) == qx && (*y >= 0.5) == qy);
            assert!(found, "象限 ({qx}, {qy}) 里一个采样点都没有");
        }
    }

    #[test]
    fn the_basis_is_orthonormal() {
        for normal in [
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::X,
            Vec3::new(0.3, 0.9, -0.2).normalize(),
        ] {
            let (t, b) = orthonormal_basis(normal);
            assert!((t.length() - 1.0).abs() < 1e-4, "切线不是单位长度");
            assert!((b.length() - 1.0).abs() < 1e-4, "副切线不是单位长度");
            assert!(t.dot(normal).abs() < 1e-4, "切线与法线不正交");
            assert!(b.dot(normal).abs() < 1e-4, "副切线与法线不正交");
        }
    }
}
