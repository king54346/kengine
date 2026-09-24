//! GIF 首帧解码。
//!
//! `image` 的 GIF 支持要拉 `gif` 这个 crate，而工作区锁定的 `image` 版本
//! 要的 `gif` 版本和现有依赖树对不上；GIF 本身又很小——一个头、一张调色板、
//! 一段 LZW。LZW 解码交给依赖树里本来就有的 `weezl`（TIFF 在用），
//! 这里只写容器那一半。
//!
//! # 支持到哪
//!
//! - 87a / 89a，全局与局部调色板，隔行扫描；
//! - 图形控制扩展里的透明色（透明像素 alpha 为 0）；
//! - **只取第一帧**，按逻辑屏幕尺寸合成（帧比屏幕小时其余部分透明）。
//!   贴图用的 GIF 基本都是单帧，动图的其余帧要引擎有「动态贴图」的概念才有用。

use crate::TextureError;

fn error(message: &str) -> TextureError {
    TextureError(format!("GIF：{message}"))
}

/// 文件头是不是 GIF。
pub(crate) fn sniff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], TextureError> {
        let slice = self
            .bytes
            .get(self.at..self.at + count)
            .ok_or_else(|| error("文件被截断"))?;
        self.at += count;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, TextureError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, TextureError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// 读一串数据子块（每块一个长度字节，长度 0 结束），拼起来返回。
    fn sub_blocks(&mut self) -> Result<Vec<u8>, TextureError> {
        let mut data = Vec::new();
        loop {
            let length = self.u8()? as usize;
            if length == 0 {
                return Ok(data);
            }
            data.extend_from_slice(self.take(length)?);
        }
    }

    fn skip_sub_blocks(&mut self) -> Result<(), TextureError> {
        loop {
            let length = self.u8()? as usize;
            if length == 0 {
                return Ok(());
            }
            self.take(length)?;
        }
    }
}

/// 解出第一帧，返回（宽，高，RGBA8）。
pub(crate) fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), TextureError> {
    if !sniff(bytes) {
        return Err(error("不是 GIF"));
    }
    let mut r = Reader { bytes, at: 6 };
    let width = r.u16()? as usize;
    let height = r.u16()? as usize;
    let flags = r.u8()?;
    let _background = r.u8()?;
    let _aspect = r.u8()?;
    if width == 0 || height == 0 || width * height > 1 << 26 {
        return Err(error("尺寸不合理"));
    }
    let global_palette = if flags & 0x80 != 0 {
        Some(r.take(3 << ((flags & 7) + 1))?)
    } else {
        None
    };

    let mut transparent: Option<u8> = None;
    loop {
        match r.u8()? {
            // 扩展块
            0x21 => {
                let label = r.u8()?;
                if label == 0xF9 {
                    let block = r.sub_blocks()?;
                    if block.len() >= 4 && block[0] & 1 != 0 {
                        transparent = Some(block[3]);
                    }
                } else {
                    r.skip_sub_blocks()?;
                }
            }
            // 图像描述符
            0x2C => {
                let left = r.u16()? as usize;
                let top = r.u16()? as usize;
                let frame_width = r.u16()? as usize;
                let frame_height = r.u16()? as usize;
                let frame_flags = r.u8()?;
                let local_palette = if frame_flags & 0x80 != 0 {
                    Some(r.take(3 << ((frame_flags & 7) + 1))?)
                } else {
                    None
                };
                let palette = local_palette
                    .or(global_palette)
                    .ok_or_else(|| error("没有调色板"))?;
                let interlaced = frame_flags & 0x40 != 0;
                let min_code_size = r.u8()?;
                if !(1..=11).contains(&min_code_size) {
                    return Err(error("LZW 码长不合法"));
                }
                let compressed = r.sub_blocks()?;
                let indices = lzw(&compressed, min_code_size.max(2), frame_width * frame_height);

                let mut rgba = vec![0u8; width * height * 4];
                for (position, &index) in indices.iter().enumerate().take(frame_width * frame_height) {
                    let (column, row) = (position % frame_width, position / frame_width);
                    let row = if interlaced { deinterlace(row, frame_height) } else { row };
                    let (x, y) = (left + column, top + row);
                    if x >= width || y >= height || Some(index) == transparent {
                        continue;
                    }
                    let Some(color) = palette.get(index as usize * 3..index as usize * 3 + 3) else {
                        continue;
                    };
                    let at = (y * width + x) * 4;
                    rgba[at..at + 3].copy_from_slice(color);
                    rgba[at + 3] = 255;
                }
                return Ok((width as u32, height as u32, rgba));
            }
            0x3B => return Err(error("没有图像帧")),
            other => return Err(error(&format!("未知的块标记 0x{other:02X}"))),
        }
    }
}

/// LZW 解码。截断或码流损坏时尽量返回已解出的部分——半张图比一张都没有强，
/// 浏览器也是这么做的。
fn lzw(data: &[u8], min_code_size: u8, expected: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(expected);
    let mut decoder = weezl::decode::Decoder::new(weezl::BitOrder::Lsb, min_code_size);
    let _ = decoder.into_vec(&mut out).decode(data);
    out
}

/// 隔行扫描：存储顺序的第 `row` 行在图里是第几行。
///
/// 四趟：每 8 行取第 0 行、每 8 行取第 4 行、每 4 行取第 2 行、每 2 行取第 1 行。
fn deinterlace(row: usize, height: usize) -> usize {
    let passes = [(0, 8), (4, 8), (2, 4), (1, 2)];
    let mut remaining = row;
    for (start, step) in passes {
        let count = if height > start { (height - start).div_ceil(step) } else { 0 };
        if remaining < count {
            return start + remaining * step;
        }
        remaining -= count;
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张 2×2、四色调色板、第 3 色透明的 GIF，LZW 码流用 weezl 现编。
    fn tiny_gif(interlaced: bool) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&[2, 0, 2, 0, 0x81, 0, 0]); // 全局调色板，4 色
        bytes.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]);
        bytes.extend_from_slice(&[0x21, 0xF9, 4, 1, 0, 0, 3, 0]); // 透明色 = 3
        bytes.push(0x2C);
        bytes.extend_from_slice(&[0, 0, 0, 0, 2, 0, 2, 0, if interlaced { 0x40 } else { 0 }]);
        bytes.push(2);
        let compressed = weezl::encode::Encoder::new(weezl::BitOrder::Lsb, 2)
            .encode(&[0, 1, 2, 3])
            .unwrap();
        for chunk in compressed.chunks(255) {
            bytes.push(chunk.len() as u8);
            bytes.extend_from_slice(chunk);
        }
        bytes.extend_from_slice(&[0, 0x3B]);
        bytes
    }

    #[test]
    fn decodes_palette_and_transparency() {
        let (w, h, rgba) = decode(&tiny_gif(false)).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
        assert_eq!(&rgba[8..12], &[0, 0, 255, 255]);
        assert_eq!(rgba[15], 0, "透明色的 alpha 应当是 0");
    }

    #[test]
    fn interlaced_rows_are_put_back_in_order() {
        // 两行的图：隔行顺序是第 0 行（第一趟）、第 1 行（第四趟），与原顺序相同。
        assert_eq!(decode(&tiny_gif(true)).unwrap().2, decode(&tiny_gif(false)).unwrap().2);
        // 8 行的图：存储顺序 0..8 → 图中行 0,4,2,6,1,3,5,7。
        let order: Vec<usize> = (0..8).map(|row| deinterlace(row, 8)).collect();
        assert_eq!(order, vec![0, 4, 2, 6, 1, 3, 5, 7]);
    }

    #[test]
    fn truncated_files_error_instead_of_panicking() {
        let gif = tiny_gif(false);
        for length in 0..gif.len() - 2 {
            let _ = decode(&gif[..length]);
        }
    }
}
