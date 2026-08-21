//! Radiance `.hdr` 解码。
//!
//! # 为什么需要真的 HDR
//!
//! 程序化天空（[`Sky`](crate::Sky)）只有天顶、地平线、地面三个颜色加一个太阳。
//! 它能给出合理的环境光，但给不出**真实场景的光照分布**——一间屋子里
//! 窗户的方向、树荫下的绿色反射、傍晚天空的渐变，这些只能从实拍的
//! 全景 HDR 里来。
//!
//! # RGBE 编码
//!
//! `.hdr` 每个像素四个字节：RGB 各一个尾数，第四个字节是**共享的指数**。
//! 真实值是 `mantissa / 256 * 2^(exponent - 128)`。
//!
//! 这么存的好处是一个像素只要 4 字节就能表示 10^-38 到 10^38 的动态范围；
//! 代价是三个通道共用指数，所以颜色差异极大时（比如纯红的高光）精度会掉。
//!
//! # 为什么自己写而不是用 crate
//!
//! `image` 已经在依赖里，但它的 HDR 支持要开一个额外的 feature，而且
//! 解码结果是它自己的类型，还要再转一道。RGBE 的格式很小——
//! 连 RLE 一起不到两百行，自己写省掉一层转换和一个 feature 开关。

use kmath::Vec3;

/// 一张解码后的 HDR 图。
///
/// 像素是**线性**的 RGB，值可以远大于 1。
#[derive(Debug, Clone, PartialEq)]
pub struct HdrImage {
    width: usize,
    height: usize,
    /// 行主序，每像素三个 `f32`。
    pixels: Vec<f32>,
}

/// 解码失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdrError(pub String);

impl std::fmt::Display for HdrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HDR 解码失败：{}", self.0)
    }
}

impl std::error::Error for HdrError {}

impl HdrImage {
    /// 宽。
    pub fn width(&self) -> usize {
        self.width
    }

    /// 高。
    pub fn height(&self) -> usize {
        self.height
    }

    /// 全部像素，行主序，每像素三个 `f32`。
    pub fn pixels(&self) -> &[f32] {
        &self.pixels
    }

    /// 取一个像素。越界时夹取。
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

    /// 按方向采样一张**等距柱状投影**（equirectangular）全景图。
    ///
    /// 这是全景 HDR 的通行格式：水平是经度 0~360°，垂直是纬度 -90~90°。
    ///
    /// 用双线性插值：最近邻在低分辨率的 HDR 上会让环境光出现明显的色块，
    /// 而环境光图通常故意存得很小（几百像素宽就够漫反射用）。
    pub fn sample_direction(&self, direction: Vec3) -> Vec3 {
        if self.width == 0 || self.height == 0 {
            return Vec3::ZERO;
        }
        let d = direction.normalize_or(Vec3::Y);

        // 经度：绕 Y 轴一圈。加 PI 是为了让 -Z（引擎的前方）落在图的中间。
        let u = (d.z.atan2(d.x) + std::f32::consts::PI) / std::f32::consts::TAU;
        // 纬度：+Y 在图的顶端。
        let v = d.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;

        let fx = u * self.width as f32 - 0.5;
        let fy = v * self.height as f32 - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let (tx, ty) = (fx - x0, fy - y0);

        // 水平方向要**环绕**：全景图的左右边是连续的，
        // 夹取的话接缝处会出现一条颜色不连续的竖线。
        let wrap_x = |x: i32| -> usize {
            let w = self.width as i32;
            (((x % w) + w) % w) as usize
        };
        let clamp_y = |y: i32| -> usize { y.clamp(0, self.height as i32 - 1) as usize };

        let (x0i, y0i) = (x0 as i32, y0 as i32);
        let p00 = self.pixel(wrap_x(x0i), clamp_y(y0i));
        let p10 = self.pixel(wrap_x(x0i + 1), clamp_y(y0i));
        let p01 = self.pixel(wrap_x(x0i), clamp_y(y0i + 1));
        let p11 = self.pixel(wrap_x(x0i + 1), clamp_y(y0i + 1));

        let top = p00.lerp(p10, tx);
        let bottom = p01.lerp(p11, tx);
        top.lerp(bottom, ty)
    }

    /// 从字节解码一张 `.hdr`。
    pub fn decode(bytes: &[u8]) -> Result<Self, HdrError> {
        let mut cursor = 0usize;

        // ── 文件头 ──
        // 以 `#?RADIANCE` 或 `#?RGBE` 开头，接若干行属性，空行结束。
        let magic = read_line(bytes, &mut cursor)?;
        if !magic.starts_with("#?") {
            return Err(HdrError(format!("不是 Radiance 文件：{magic:?}")));
        }
        loop {
            let line = read_line(bytes, &mut cursor)?;
            if line.is_empty() {
                break;
            }
            // 只认线性 RGBE。`FORMAT=32-bit_rle_xyze` 是 CIE XYZ，
            // 直接当 RGB 用会得到完全错误的颜色。
            if let Some(format) = line.strip_prefix("FORMAT=")
                && format.trim() != "32-bit_rle_rgbe"
            {
                return Err(HdrError(format!("不支持的格式：{format}")));
            }
        }

        // ── 分辨率 ──
        // 通行写法是 `-Y height +X width`（从上往下、从左往右）。
        let resolution = read_line(bytes, &mut cursor)?;
        let parts: Vec<&str> = resolution.split_whitespace().collect();
        if parts.len() != 4 || parts[0] != "-Y" || parts[2] != "+X" {
            return Err(HdrError(format!("不支持的分辨率行：{resolution:?}")));
        }
        let height: usize = parts[1]
            .parse()
            .map_err(|_| HdrError(format!("高度无效：{}", parts[1])))?;
        let width: usize = parts[3]
            .parse()
            .map_err(|_| HdrError(format!("宽度无效：{}", parts[3])))?;
        if width == 0 || height == 0 {
            return Err(HdrError("尺寸为零".into()));
        }
        // 上限挡一下：损坏的文件头能声称自己有几十亿像素，
        // 照着分配会直接 OOM。
        if width > 65536 || height > 65536 {
            return Err(HdrError(format!("尺寸过大：{width}×{height}")));
        }

        let mut pixels = vec![0.0f32; width * height * 3];
        let mut scanline = vec![[0u8; 4]; width];

        for row in 0..height {
            read_scanline(bytes, &mut cursor, &mut scanline)?;
            for (col, rgbe) in scanline.iter().enumerate() {
                let color = rgbe_to_linear(*rgbe);
                let index = (row * width + col) * 3;
                pixels[index] = color.x;
                pixels[index + 1] = color.y;
                pixels[index + 2] = color.z;
            }
        }

        Ok(Self {
            width,
            height,
            pixels,
        })
    }
}

/// RGBE 一个像素转线性 RGB。
fn rgbe_to_linear(rgbe: [u8; 4]) -> Vec3 {
    // 指数为 0 表示这个像素是纯黑。**不能**按公式算——
    // `2^(0-128)` 是一个极小但非零的数，一整片黑天空会变成
    // 一片有噪点的深灰。
    if rgbe[3] == 0 {
        return Vec3::ZERO;
    }
    let scale = libm_exp2(rgbe[3] as i32 - 128 - 8);
    Vec3::new(
        rgbe[0] as f32 * scale,
        rgbe[1] as f32 * scale,
        rgbe[2] as f32 * scale,
    )
}

/// `2^n`，`n` 是整数。
///
/// 手写而不是 `powi`：指数范围是 -136..=127，`powi` 在这个范围上
/// 要走通用的幂运算，而这里只是构造一个浮点数。
fn libm_exp2(n: i32) -> f32 {
    // 直接构造 IEEE754 的指数位。范围外的走保守路径。
    if (-126..=127).contains(&n) {
        f32::from_bits(((n + 127) as u32) << 23)
    } else {
        (2.0f32).powi(n)
    }
}

/// 读一行（到 `\n` 为止，不含换行符）。
fn read_line(bytes: &[u8], cursor: &mut usize) -> Result<String, HdrError> {
    let start = *cursor;
    while *cursor < bytes.len() && bytes[*cursor] != b'\n' {
        *cursor += 1;
    }
    if *cursor >= bytes.len() {
        return Err(HdrError("文件在读完头之前就结束了".into()));
    }
    let line = String::from_utf8_lossy(&bytes[start..*cursor]).into_owned();
    *cursor += 1; // 跳过换行符
    Ok(line)
}

/// 读一整行像素，自动处理 RLE 与非 RLE 两种编码。
fn read_scanline(bytes: &[u8], cursor: &mut usize, out: &mut [[u8; 4]]) -> Result<(), HdrError> {
    let width = out.len();
    if *cursor + 4 > bytes.len() {
        return Err(HdrError("像素数据不完整".into()));
    }

    let header = [
        bytes[*cursor],
        bytes[*cursor + 1],
        bytes[*cursor + 2],
        bytes[*cursor + 3],
    ];

    // 新式 RLE 的标志：前两字节是 2、2，后两字节拼出宽度。
    // 宽度必须在 8..=32767 之间，否则这四个字节是普通像素。
    let rle_width = ((header[2] as usize) << 8) | header[3] as usize;
    let is_rle = header[0] == 2 && header[1] == 2 && (8..=32767).contains(&rle_width);

    if !is_rle {
        // 平铺（或旧式 RLE）。这里只支持平铺——旧式 RLE 极少见，
        // 而且和平铺混在一起解析很容易出错。
        for pixel in out.iter_mut() {
            if *cursor + 4 > bytes.len() {
                return Err(HdrError("像素数据不完整".into()));
            }
            *pixel = [
                bytes[*cursor],
                bytes[*cursor + 1],
                bytes[*cursor + 2],
                bytes[*cursor + 3],
            ];
            *cursor += 4;
        }
        return Ok(());
    }

    if rle_width != width {
        return Err(HdrError(format!(
            "RLE 行宽 {rle_width} 与图像宽度 {width} 不符"
        )));
    }
    *cursor += 4;

    // 新式 RLE 是**按通道**存的：先一整行的 R，再一整行的 G……
    // 按像素解会得到一张颜色完全错乱的图。
    for channel in 0..4 {
        let mut filled = 0usize;
        while filled < width {
            if *cursor >= bytes.len() {
                return Err(HdrError("RLE 数据不完整".into()));
            }
            let count = bytes[*cursor] as usize;
            *cursor += 1;

            if count > 128 {
                // 游程：接下来一个字节重复 `count - 128` 次。
                let run = count - 128;
                if *cursor >= bytes.len() || filled + run > width {
                    return Err(HdrError("RLE 游程越界".into()));
                }
                let value = bytes[*cursor];
                *cursor += 1;
                for pixel in out.iter_mut().skip(filled).take(run) {
                    pixel[channel] = value;
                }
                filled += run;
            } else {
                // 直出：接下来 `count` 个字节各用一次。
                // count 为 0 是非法的，会导致死循环。
                if count == 0 {
                    return Err(HdrError("RLE 出现了零长度的段".into()));
                }
                if *cursor + count > bytes.len() || filled + count > width {
                    return Err(HdrError("RLE 直出段越界".into()));
                }
                for offset in 0..count {
                    out[filled + offset][channel] = bytes[*cursor + offset];
                }
                *cursor += count;
                filled += count;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张平铺（非 RLE）的 `.hdr`。
    fn flat_hdr(width: usize, height: usize, pixel: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n");
        bytes.extend_from_slice(b"FORMAT=32-bit_rle_rgbe\n");
        bytes.extend_from_slice(b"\n");
        bytes.extend_from_slice(format!("-Y {height} +X {width}\n").as_bytes());
        for _ in 0..width * height {
            bytes.extend_from_slice(&pixel);
        }
        bytes
    }

    /// 造一张 RLE 的 `.hdr`，每行是同一个像素。
    fn rle_hdr(width: usize, height: usize, pixel: [u8; 4]) -> Vec<u8> {
        assert!((8..=32767).contains(&width));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n");
        bytes.extend_from_slice(b"FORMAT=32-bit_rle_rgbe\n");
        bytes.extend_from_slice(b"\n");
        bytes.extend_from_slice(format!("-Y {height} +X {width}\n").as_bytes());

        for _ in 0..height {
            bytes.extend_from_slice(&[2, 2, (width >> 8) as u8, (width & 0xff) as u8]);
            for channel in 0..4 {
                // 一个游程覆盖整行。游程最长 127。
                let mut remaining = width;
                while remaining > 0 {
                    let run = remaining.min(127);
                    bytes.push((128 + run) as u8);
                    bytes.push(pixel[channel]);
                    remaining -= run;
                }
            }
        }
        bytes
    }

    #[test]
    fn a_flat_image_decodes() {
        // 尾数 128、指数 128 → 128/256 * 2^0 = 0.5
        let bytes = flat_hdr(4, 3, [128, 128, 128, 128]);
        let image = HdrImage::decode(&bytes).expect("该能解码");

        assert_eq!((image.width(), image.height()), (4, 3));
        let p = image.pixel(0, 0);
        assert!((p.x - 0.5).abs() < 1e-5, "解出来是 {p:?}");
        assert_eq!(p.x, p.y);
        assert_eq!(p.y, p.z);
    }

    #[test]
    fn an_rle_image_decodes_to_the_same_thing() {
        // 新式 RLE 是按**通道**存的。按像素解会得到颜色完全错乱的图。
        let flat = HdrImage::decode(&flat_hdr(16, 4, [200, 100, 50, 130])).unwrap();
        let rle = HdrImage::decode(&rle_hdr(16, 4, [200, 100, 50, 130])).unwrap();
        assert_eq!(flat, rle);
    }

    #[test]
    fn rle_preserves_channel_order() {
        let image = HdrImage::decode(&rle_hdr(16, 2, [255, 0, 0, 128])).unwrap();
        let p = image.pixel(5, 1);
        assert!(p.x > 0.0, "红通道该有值");
        assert_eq!(p.y, 0.0, "绿通道该是 0");
        assert_eq!(p.z, 0.0, "蓝通道该是 0");
    }

    #[test]
    fn a_zero_exponent_is_pure_black() {
        // 按公式算的话 `2^(0-128)` 是极小但非零，
        // 一整片黑天空会变成有噪点的深灰。
        let image = HdrImage::decode(&flat_hdr(4, 4, [255, 255, 255, 0])).unwrap();
        assert_eq!(image.pixel(1, 1), Vec3::ZERO);
    }

    #[test]
    fn values_can_exceed_one() {
        // HDR 的全部意义：太阳可以是 1000。
        // 指数 128 + 10 → 尾数 × 2^10 / 256。
        let image = HdrImage::decode(&flat_hdr(4, 4, [255, 255, 255, 138])).unwrap();
        assert!(image.pixel(0, 0).x > 100.0, "实测 {}", image.pixel(0, 0).x);
    }

    #[test]
    fn a_non_radiance_file_is_rejected() {
        assert!(HdrImage::decode(b"not an hdr file\n").is_err());
        assert!(HdrImage::decode(&[]).is_err());
    }

    #[test]
    fn an_xyz_file_is_rejected() {
        // CIE XYZ 直接当 RGB 用会得到完全错误的颜色，
        // 而且不会有任何报错——所以必须在这里拦住。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n");
        bytes.extend_from_slice(b"FORMAT=32-bit_rle_xyze\n");
        bytes.extend_from_slice(b"\n-Y 4 +X 4\n");
        assert!(HdrImage::decode(&bytes).is_err());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicking() {
        let full = flat_hdr(8, 8, [128, 128, 128, 128]);
        for cut in [10, 30, full.len() / 2, full.len() - 1] {
            let result = HdrImage::decode(&full[..cut]);
            assert!(result.is_err(), "截断到 {cut} 字节时该报错");
        }
    }

    #[test]
    fn an_absurd_size_is_rejected_before_allocating() {
        // 损坏的文件头能声称自己有几十亿像素，照着分配会直接 OOM。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n\n-Y 999999 +X 999999\n");
        assert!(HdrImage::decode(&bytes).is_err());
    }

    #[test]
    fn a_zero_length_rle_run_does_not_hang() {
        // count 为 0 会让循环永远填不满一行。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n\n-Y 1 +X 16\n");
        bytes.extend_from_slice(&[2, 2, 0, 16]);
        bytes.push(0); // 非法的零长度段
        assert!(HdrImage::decode(&bytes).is_err());
    }

    #[test]
    fn sampling_wraps_horizontally() {
        // 全景图的左右边是连续的。夹取的话接缝处会出现一条
        // 颜色不连续的竖线。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 2 +X 4\n");
        // 一行四个像素，第一个和最后一个颜色相同。
        for _ in 0..2 {
            bytes.extend_from_slice(&[255, 0, 0, 128]);
            bytes.extend_from_slice(&[0, 255, 0, 128]);
            bytes.extend_from_slice(&[0, 0, 255, 128]);
            bytes.extend_from_slice(&[255, 0, 0, 128]);
        }
        let image = HdrImage::decode(&bytes).unwrap();

        // 绕一整圈回到同一个方向，采样结果必须一致。
        let a = image.sample_direction(Vec3::new(1.0, 0.0, 0.0));
        let b = image.sample_direction(Vec3::new(1.0, 0.0, 1e-6));
        assert!((a - b).length() < 0.1, "接缝处不连续：{a:?} vs {b:?}");
    }

    #[test]
    fn up_and_down_sample_the_poles() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 4 +X 4\n");
        // 上两行亮、下两行暗。
        for row in 0..4 {
            let value = if row < 2 { 255 } else { 16 };
            for _ in 0..4 {
                bytes.extend_from_slice(&[value, value, value, 128]);
            }
        }
        let image = HdrImage::decode(&bytes).unwrap();

        let up = image.sample_direction(Vec3::Y);
        let down = image.sample_direction(Vec3::NEG_Y);
        assert!(up.x > down.x, "+Y 该采到图的顶端：{up:?} vs {down:?}");
    }

    #[test]
    fn sampling_an_empty_image_is_black_not_a_panic() {
        let image = HdrImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
        assert_eq!(image.sample_direction(Vec3::Y), Vec3::ZERO);
    }

    #[test]
    fn exp2_matches_powi() {
        for n in [-140, -126, -50, 0, 10, 100, 127, 130] {
            let expected = (2.0f32).powi(n);
            let actual = libm_exp2(n);
            // 130 超出 f32 的指数范围，两边都是 inf——
            // 直接相减会得到 NaN，比较就永远为假。
            if !expected.is_finite() {
                assert_eq!(actual.is_infinite(), expected.is_infinite(), "2^{n}");
                continue;
            }
            assert!(
                (actual - expected).abs() <= expected.abs() * 1e-6,
                "2^{n}：{actual} vs {expected}"
            );
        }
    }
}
