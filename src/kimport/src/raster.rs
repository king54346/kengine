//! 一个小的 CPU 光栅器：把 [`Tessellation`] 画进 RGBA 画布。
//!
//! Lottie 动画、SVG 转贴图这类「矢量图 → 每帧一张纹理」的需求，浏览器里
//! 有 Canvas 2D 现成用；引擎这边没有，这里补一个够用的。
//!
//! # 抗锯齿
//!
//! 每个像素 4×4 = 16 个采样点，每个点一位。同一次 [`Canvas::fill`] 里的
//! 三角形先把位**按位或**进覆盖掩码，最后按命中的位数算覆盖率、只合成
//! 一次——描边的四边形和拐角补丁互相重叠也不会叠深，半透明描边是均匀的。
//!
//! # 颜色
//!
//! 画布里存的是**预乘**的 RGBA（f32）。颜色按调用方给的空间原样混合——
//! 和浏览器的 Canvas 一样，Lottie / SVG 的颜色是 sRGB，就在 sRGB 里混。
//!

use crate::path::Tessellation;
use kmath::{Affine2, Vec2};
use ktexture::{Texture, TextureFormat};

/// 遮罩怎么用（Lottie 的 track matte）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatteMode {
    /// 按遮罩的不透明度。
    Alpha,
    /// 按遮罩不透明度的反相。
    AlphaInverted,
    /// 按遮罩的亮度。
    Luma,
    /// 按遮罩亮度的反相。
    LumaInverted,
}

/// 一张 RGBA 画布。
#[derive(Debug, Clone)]
pub struct Canvas {
    width: usize,
    height: usize,
    /// 预乘的 RGBA。
    pixels: Vec<[f32; 4]>,
    /// 每像素 16 位的覆盖掩码，`fill` 期间用，用完清零。
    mask: Vec<u16>,
}

/// 16 个采样点在像素里的位置（4×4 均匀格）。
const SAMPLES: [f32; 4] = [0.125, 0.375, 0.625, 0.875];

impl Canvas {
    /// 一张全透明的画布。
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![[0.0; 4]; width * height],
            mask: vec![0; width * height],
        }
    }

    /// 宽。
    pub fn width(&self) -> usize {
        self.width
    }

    /// 高。
    pub fn height(&self) -> usize {
        self.height
    }

    /// 清成全透明。
    pub fn clear(&mut self) {
        self.pixels.fill([0.0; 4]);
    }

    /// 某个像素（预乘 RGBA）。
    pub fn pixel(&self, x: usize, y: usize) -> [f32; 4] {
        self.pixels[y * self.width + x]
    }

    /// 用一种颜色（非预乘的 RGBA）填充三角形，点先过 `transform`。
    pub fn fill(&mut self, tessellation: &Tessellation, transform: Affine2, color: [f32; 4]) {
        if tessellation.is_empty() || color[3] <= 0.0 {
            return;
        }
        let (w, h) = (self.width as i64, self.height as i64);
        let mut touched = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        for tri in tessellation.indices.chunks_exact(3) {
            let [a, b, c] =
                [0, 1, 2].map(|k| transform.transform_point2(tessellation.points[tri[k] as usize]));
            let area = (b - a).perp_dot(c - a);
            if area.abs() < 1e-9 || !area.is_finite() {
                continue;
            }
            let (a, b, c) = if area > 0.0 { (a, b, c) } else { (a, c, b) };
            let x0 = (a.x.min(b.x).min(c.x).floor() as i64).max(0);
            let y0 = (a.y.min(b.y).min(c.y).floor() as i64).max(0);
            let x1 = (a.x.max(b.x).max(c.x).ceil() as i64).min(w - 1);
            let y1 = (a.y.max(b.y).max(c.y).ceil() as i64).min(h - 1);
            if x0 > x1 || y0 > y1 {
                continue;
            }
            touched = [
                touched[0].min(x0),
                touched[1].min(y0),
                touched[2].max(x1),
                touched[3].max(y1),
            ];
            let edge = |p: Vec2, q: Vec2, s: Vec2| (q - p).perp_dot(s - p);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let mut bits = 0u16;
                    for (j, sy) in SAMPLES.iter().enumerate() {
                        for (i, sx) in SAMPLES.iter().enumerate() {
                            let s = Vec2::new(x as f32 + sx, y as f32 + sy);
                            if edge(a, b, s) >= 0.0 && edge(b, c, s) >= 0.0 && edge(c, a, s) >= 0.0
                            {
                                bits |= 1 << (j * 4 + i);
                            }
                        }
                    }
                    self.mask[(y * w + x) as usize] |= bits;
                }
            }
        }
        if touched[0] > touched[2] {
            return;
        }
        let alpha = color[3].clamp(0.0, 1.0);
        for y in touched[1]..=touched[3] {
            for x in touched[0]..=touched[2] {
                let index = (y * w + x) as usize;
                let bits = std::mem::take(&mut self.mask[index]);
                if bits == 0 {
                    continue;
                }
                let a = alpha * bits.count_ones() as f32 / 16.0;
                let dst = &mut self.pixels[index];
                for k in 0..3 {
                    dst[k] = color[k] * a + dst[k] * (1.0 - a);
                }
                dst[3] = a + dst[3] * (1.0 - a);
            }
        }
    }

    /// 把另一张同尺寸的画布叠上来（source-over），可选乘一个遮罩。
    pub fn composite(
        &mut self,
        source: &Canvas,
        opacity: f32,
        matte: Option<(&Canvas, MatteMode)>,
    ) {
        let count = self.pixels.len().min(source.pixels.len());
        for index in 0..count {
            let src = source.pixels[index];
            if src[3] <= 0.0 {
                continue;
            }
            let mut factor = opacity;
            if let Some((matte, mode)) = matte {
                let m = matte.pixels[index];
                let luma = 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
                factor *= match mode {
                    MatteMode::Alpha => m[3],
                    MatteMode::AlphaInverted => 1.0 - m[3],
                    MatteMode::Luma => luma,
                    MatteMode::LumaInverted => 1.0 - luma,
                };
            }
            if factor <= 0.0 {
                continue;
            }
            let a = src[3] * factor;
            let dst = &mut self.pixels[index];
            for k in 0..3 {
                dst[k] = src[k] * factor + dst[k] * (1.0 - a);
            }
            dst[3] = a + dst[3] * (1.0 - a);
        }
    }

    /// 转成 RGBA8 纹理（反预乘）。`srgb` 指定纹理按 sRGB 还是线性解读。
    pub fn to_texture(&self, srgb: bool) -> Texture {
        let mut data = Vec::with_capacity(self.pixels.len() * 4);
        for p in &self.pixels {
            let a = p[3].clamp(0.0, 1.0);
            let un = |c: f32| {
                if a > 0.0 {
                    (c / a).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            };
            data.extend_from_slice(&[
                (un(p[0]) * 255.0 + 0.5) as u8,
                (un(p[1]) * 255.0 + 0.5) as u8,
                (un(p[2]) * 255.0 + 0.5) as u8,
                (a * 255.0 + 0.5) as u8,
            ]);
        }
        Texture::new(self.width as u32, self.height as u32, data).with_format(if srgb {
            TextureFormat::Srgb
        } else {
            TextureFormat::Linear
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(x0: f32, y0: f32, x1: f32, y1: f32) -> Tessellation {
        Tessellation {
            points: vec![
                Vec2::new(x0, y0),
                Vec2::new(x1, y0),
                Vec2::new(x1, y1),
                Vec2::new(x0, y1),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        }
    }

    #[test]
    fn a_pixel_aligned_square_is_fully_covered_and_edges_are_half() {
        let mut canvas = Canvas::new(8, 8);
        canvas.fill(
            &quad(2.0, 2.0, 4.5, 4.0),
            Affine2::IDENTITY,
            [1.0, 0.0, 0.0, 1.0],
        );
        assert_eq!(canvas.pixel(2, 2), [1.0, 0.0, 0.0, 1.0]);
        assert!((canvas.pixel(4, 2)[3] - 0.5).abs() < 1e-6);
        assert_eq!(canvas.pixel(6, 6)[3], 0.0);
    }

    #[test]
    fn overlapping_triangles_in_one_fill_do_not_double_blend() {
        let mut canvas = Canvas::new(4, 4);
        let mut twice = quad(0.0, 0.0, 4.0, 4.0);
        twice.points.extend(quad(0.0, 0.0, 4.0, 4.0).points);
        twice.indices.extend([4, 5, 6, 4, 6, 7]);
        canvas.fill(&twice, Affine2::IDENTITY, [1.0, 1.0, 1.0, 0.5]);
        assert!((canvas.pixel(1, 1)[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn inverted_alpha_matte_cuts_out() {
        let mut matte = Canvas::new(4, 1);
        matte.fill(&quad(0.0, 0.0, 2.0, 1.0), Affine2::IDENTITY, [1.0; 4]);
        let mut layer = Canvas::new(4, 1);
        layer.fill(
            &quad(0.0, 0.0, 4.0, 1.0),
            Affine2::IDENTITY,
            [0.0, 1.0, 0.0, 1.0],
        );
        let mut out = Canvas::new(4, 1);
        out.composite(&layer, 1.0, Some((&matte, MatteMode::AlphaInverted)));
        assert_eq!(out.pixel(0, 0)[3], 0.0);
        assert_eq!(out.pixel(3, 0)[3], 1.0);
    }

    #[test]
    fn texture_is_unpremultiplied() {
        let mut canvas = Canvas::new(1, 1);
        canvas.fill(
            &quad(0.0, 0.0, 1.0, 1.0),
            Affine2::IDENTITY,
            [1.0, 0.0, 0.0, 0.5],
        );
        let texture = canvas.to_texture(true);
        assert_eq!(&texture.data()[..4], &[255, 0, 0, 128]);
    }
}
