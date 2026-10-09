//! 3D 颜色查找表（LUT）：调色师在达芬奇 / Photoshop 里调好的颜色，
//! 导出成一张表，游戏里逐像素查。
//!
//! 三种来源，读进来都是同一个 [`Lut3d`]：
//!
//! | 格式 | 从哪来 | 数据 |
//! |---|---|---|
//! | `.cube` | Adobe / Resolve 的标准导出 | 浮点，红最快 |
//! | `.3dl` | Autodesk / 老的调色软件 | 整数（10 或 12 位），**蓝**最快 |
//! | 条带图 | Unreal、Unity、three.js 的 `LUTImageLoader` | `N² × N` 的 PNG，x = r + b·N，y = g |
//!
//! GPU 那边不用 3D 纹理，而是把表摊成一张 `N² × N` 的 2D **条带**
//! （[`Lut3d::to_strip`]），在着色器里手动在两片之间插值——
//! 这是 Unreal 的做法，好处是任何能采 2D 纹理的地方都能用。

use crate::{FilterMode, Sampler, Texture, TextureError, TextureFormat, WrapMode};

/// 一张 3D LUT。`data[r + g·N + b·N²]` 是输入 `(r, g, b) / (N-1)` 对应的输出。
#[derive(Debug, Clone, PartialEq)]
pub struct Lut3d {
    size: u32,
    data: Vec<[f32; 3]>,
}

impl Lut3d {
    /// 恒等表：输出等于输入。
    pub fn identity(size: u32) -> Self {
        let size = size.max(2);
        let scale = 1.0 / (size - 1) as f32;
        let mut data = Vec::with_capacity((size * size * size) as usize);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 * scale, g as f32 * scale, b as f32 * scale]);
                }
            }
        }
        Self { size, data }
    }

    /// 每条边几格。
    pub fn size(&self) -> u32 {
        self.size
    }

    /// 原始数据，红最快。
    pub fn data(&self) -> &[[f32; 3]] {
        &self.data
    }

    /// 读 `.cube`。只认 3D 表；`DOMAIN_MIN` / `DOMAIN_MAX` 会被换算掉。
    pub fn parse_cube(text: &str) -> Result<Self, TextureError> {
        let mut size = 0u32;
        let mut domain_min = [0.0f32; 3];
        let mut domain_max = [1.0f32; 3];
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or_default();
            match first {
                "LUT_3D_SIZE" => {
                    size = words
                        .next()
                        .and_then(|w| w.parse().ok())
                        .ok_or_else(|| TextureError("LUT_3D_SIZE 后面不是数字".into()))?;
                }
                "LUT_1D_SIZE" => return Err(TextureError("只支持 3D LUT，这是一张 1D 表".into())),
                "DOMAIN_MIN" => domain_min = parse_triple(words)?,
                "DOMAIN_MAX" => domain_max = parse_triple(words)?,
                "TITLE" | "LUT_3D_INPUT_RANGE" => {}
                _ => {
                    if first.starts_with(|c: char| c.is_ascii_alphabetic()) {
                        // 未知的关键字：跳过，别的软件经常塞点私货进来。
                        continue;
                    }
                    let rest: Vec<&str> = std::iter::once(first).chain(words).collect();
                    let triple = parse_triple(rest.into_iter())?;
                    data.push(triple);
                }
            }
        }
        if size < 2 {
            return Err(TextureError(".cube 里没有 LUT_3D_SIZE".into()));
        }
        let expected = (size * size * size) as usize;
        if data.len() != expected {
            return Err(TextureError(format!(
                ".cube 声明了 {size}³ = {expected} 个点，实际有 {}",
                data.len()
            )));
        }
        // 把定义域换算回 [0, 1]：表里的输出值是按输入域给的，这里只影响输出不需要动；
        // 真正要换算的是**输入**——但表是规则网格，输入域只决定网格点落在哪。
        // 绝大多数文件都是 0..1，偏离的时候把输出按同样的比例拉回来，
        // 让「恒等」依然是恒等。
        let span = [
            (domain_max[0] - domain_min[0]).max(1e-6),
            (domain_max[1] - domain_min[1]).max(1e-6),
            (domain_max[2] - domain_min[2]).max(1e-6),
        ];
        for value in &mut data {
            for c in 0..3 {
                value[c] = (value[c] - domain_min[c]) / span[c];
            }
        }
        Ok(Self { size, data })
    }

    /// 读 `.3dl`。第一行是输入刻度（N 个整数），之后每行一个输出点，蓝最快。
    /// 位深从最大值推：1023 以内按 10 位，4095 以内按 12 位，否则 16 位。
    pub fn parse_3dl(text: &str) -> Result<Self, TextureError> {
        let mut rows: Vec<Vec<f32>> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let values: Vec<f32> = line
                .split_whitespace()
                .filter_map(|w| w.parse::<f32>().ok())
                .collect();
            if !values.is_empty() {
                rows.push(values);
            }
        }
        // 第一行是刻度（N 个数，不是三个一组的点）。
        let (size, points) = match rows.first() {
            Some(first) if first.len() != 3 => (first.len() as u32, &rows[1..]),
            _ => {
                // 没有刻度行：按点数开立方。
                let size = (rows.len() as f64).cbrt().round() as u32;
                (size, &rows[..])
            }
        };
        let expected = (size * size * size) as usize;
        if size < 2 || points.len() < expected {
            return Err(TextureError(format!(
                ".3dl 需要 {expected} 个点，实际有 {}",
                points.len()
            )));
        }
        let max = points
            .iter()
            .take(expected)
            .flat_map(|row| row.iter().copied())
            .fold(0.0f32, f32::max);
        let full_scale = if max <= 1023.0 {
            1023.0
        } else if max <= 4095.0 {
            4095.0
        } else {
            65535.0
        };
        let mut data = vec![[0.0f32; 3]; expected];
        for (index, row) in points.iter().take(expected).enumerate() {
            if row.len() < 3 {
                return Err(TextureError(format!(".3dl 第 {index} 个点不足三个数")));
            }
            // 文件里蓝最快：index = b + g·N + r·N²。
            let n = size as usize;
            let b = index % n;
            let g = (index / n) % n;
            let r = index / (n * n);
            data[r + g * n + b * n * n] = [
                row[0] / full_scale,
                row[1] / full_scale,
                row[2] / full_scale,
            ];
        }
        Ok(Self { size, data })
    }

    /// 从条带图读：横条（`N² × N`）或竖条（`N × N²`）。
    pub fn from_strip(texture: &Texture) -> Result<Self, TextureError> {
        let (w, h) = (texture.width(), texture.height());
        let pixels = texture.data();
        let texel = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [
                pixels[i] as f32 / 255.0,
                pixels[i + 1] as f32 / 255.0,
                pixels[i + 2] as f32 / 255.0,
            ]
        };
        let mut data;
        let size;
        if w == h * h {
            size = h;
            data = vec![[0.0; 3]; (size * size * size) as usize];
            for b in 0..size {
                for g in 0..size {
                    for r in 0..size {
                        data[(r + g * size + b * size * size) as usize] = texel(r + b * size, g);
                    }
                }
            }
        } else if h == w * w {
            size = w;
            data = vec![[0.0; 3]; (size * size * size) as usize];
            for b in 0..size {
                for g in 0..size {
                    for r in 0..size {
                        data[(r + g * size + b * size * size) as usize] = texel(r, g + b * size);
                    }
                }
            }
        } else {
            return Err(TextureError(format!(
                "LUT 条带图的尺寸不对：{w}×{h}（要 N²×N 或 N×N²）"
            )));
        }
        Ok(Self { size, data })
    }

    /// 摊成一张 `N² × N` 的条带，线性格式、双线性、夹边。
    pub fn to_strip(&self) -> Texture {
        let n = self.size;
        let mut bytes = vec![0u8; (n * n * n * 4) as usize];
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let value = self.data[(r + g * n + b * n * n) as usize];
                    let x = r + b * n;
                    let i = ((g * n * n + x) * 4) as usize;
                    for c in 0..3 {
                        bytes[i + c] = (value[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                    bytes[i + 3] = 255;
                }
            }
        }
        Texture::new(n * n, n, bytes)
            .with_format(TextureFormat::Linear)
            .with_sampler(Sampler {
                mag_filter: FilterMode::Linear,
                min_filter: FilterMode::Linear,
                wrap_u: WrapMode::ClampToEdge,
                wrap_v: WrapMode::ClampToEdge,
                ..Default::default()
            })
    }

    /// 三线性查表。和着色器里那一份同一个算法，测试拿它对拍。
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size as usize;
        let scale = (n - 1) as f32;
        let p = rgb.map(|c| c.clamp(0.0, 1.0) * scale);
        let i0 = p.map(|c| (c.floor() as usize).min(n - 1));
        let i1 = i0.map(|c| (c + 1).min(n - 1));
        let f = [
            p[0] - i0[0] as f32,
            p[1] - i0[1] as f32,
            p[2] - i0[2] as f32,
        ];
        let at = |r: usize, g: usize, b: usize| self.data[r + g * n + b * n * n];
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        };
        let c00 = lerp(at(i0[0], i0[1], i0[2]), at(i1[0], i0[1], i0[2]), f[0]);
        let c10 = lerp(at(i0[0], i1[1], i0[2]), at(i1[0], i1[1], i0[2]), f[0]);
        let c01 = lerp(at(i0[0], i0[1], i1[2]), at(i1[0], i0[1], i1[2]), f[0]);
        let c11 = lerp(at(i0[0], i1[1], i1[2]), at(i1[0], i1[1], i1[2]), f[0]);
        lerp(lerp(c00, c10, f[1]), lerp(c01, c11, f[1]), f[2])
    }
}

fn parse_triple<'a>(mut words: impl Iterator<Item = &'a str>) -> Result<[f32; 3], TextureError> {
    let mut out = [0.0; 3];
    for value in &mut out {
        *value = words
            .next()
            .and_then(|w| w.parse().ok())
            .ok_or_else(|| TextureError("LUT 数据行不是三个数".into()))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (x - y).abs() < 1.0 / 255.0 + 1e-4)
    }

    #[test]
    fn the_identity_table_is_the_identity() {
        let lut = Lut3d::identity(17);
        for rgb in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.3, 0.6, 0.9],
            [0.51, 0.02, 0.77],
        ] {
            assert!(
                close(lut.sample(rgb), rgb),
                "{rgb:?} → {:?}",
                lut.sample(rgb)
            );
        }
    }

    #[test]
    fn a_cube_file_is_read_red_fastest() {
        // 2³：第二个点（r=1, g=0, b=0）改成纯红以外的东西，看它落在哪。
        let mut text = String::from("TITLE \"t\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{r} {g} {b}\n"));
                }
            }
        }
        let lut = Lut3d::parse_cube(&text).unwrap();
        assert_eq!(lut.size(), 2);
        assert_eq!(lut.data()[1], [1.0, 0.0, 0.0]);
        assert_eq!(lut.data()[2], [0.0, 1.0, 0.0]);
        assert_eq!(lut.data()[4], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_3dl_file_is_read_blue_fastest() {
        // 2³，10 位：蓝最快。第二行是 (r=0, g=0, b=1)。
        let mut text = String::from("0 1023\n");
        for r in 0..2 {
            for g in 0..2 {
                for b in 0..2 {
                    text.push_str(&format!("{} {} {}\n", r * 1023, g * 1023, b * 1023));
                }
            }
        }
        let lut = Lut3d::parse_3dl(&text).unwrap();
        assert!(close(lut.sample([0.0, 0.0, 1.0]), [0.0, 0.0, 1.0]));
        assert!(close(lut.sample([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]));
        assert!(close(lut.sample([0.2, 0.7, 0.4]), [0.2, 0.7, 0.4]));
    }

    #[test]
    fn a_strip_round_trips() {
        let lut = Lut3d::identity(8);
        let strip = lut.to_strip();
        assert_eq!((strip.width(), strip.height()), (64, 8));
        let back = Lut3d::from_strip(&strip).unwrap();
        for rgb in [[0.25, 0.5, 0.75], [1.0, 0.0, 0.5]] {
            assert!(close(back.sample(rgb), rgb));
        }
    }

    #[test]
    fn a_wrongly_sized_strip_is_rejected() {
        assert!(Lut3d::from_strip(&Texture::solid(10, 3, [0; 4])).is_err());
    }
}
