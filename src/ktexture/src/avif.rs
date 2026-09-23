//! AVIF 解码：容器交给 `avif-parse`，AV1 帧交给 `rav1d`，YUV→RGB 在这里做。
//!
//! # 为什么是这两个 crate
//!
//! `image` 的 AVIF 解码走的是 C 写的 dav1d（要 pkg-config 找系统库），
//! 这台引擎的规矩是纯 Rust。`rav1d` 是 dav1d 的逐行 Rust 移植，关掉
//! `asm` 特性后没有任何 C / 汇编依赖；`avif-parse` 是 Mozilla 维护的
//! HEIF 容器解析器。两者之间只差一步：dav1d 输出的是 YUV 平面，要自己
//! 按序列头里的矩阵系数和量化范围转成 RGB。
//!
//! # 支持
//!
//! - 8 / 10 / 12 位，4:0:0 / 4:2:0 / 4:2:2 / 4:4:4
//! - BT.601 / BT.709 / BT.2020 / 恒等（GBR）矩阵，全范围与有限范围
//! - 独立的 alpha 图层（`auxl`），以及预乘 alpha
//!
//! 不支持动画 AVIF（`avis`）——那是视频，不是贴图。高位深的内容直接
//! 缩到 8 位，HDR 传递函数（PQ / HLG）不做色调映射。
//!
//! # unsafe
//!
//! `rav1d` 目前只公开 dav1d 的 C ABI（`dav1d_open` / `dav1d_send_data` /
//! `dav1d_get_picture`……），所以这里有一层 `unsafe` 的薄封装。每个调用都
//! 只传本函数栈上的对象，解码结束前全部释放，指针不逃出这个模块。

use crate::TextureError;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data,
};
use std::mem::MaybeUninit;
use std::ptr::NonNull;

fn err(message: impl Into<String>) -> TextureError {
    TextureError(message.into())
}

/// 文件头是不是 AVIF（`ftyp` 盒子的主品牌是 `avif` 或兼容品牌里有它）。
pub(crate) fn sniff(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && {
        let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let end = size.min(bytes.len());
        (8..end.saturating_sub(3))
            .step_by(4)
            .any(|at| &bytes[at..at + 4] == b"avif")
    }
}

/// 一个解出来的 AV1 帧：原位深的 Y / U / V 平面（8 位也存成 `u16`）。
struct Frame {
    width: usize,
    height: usize,
    layout: u32,
    /// 矩阵系数（ITU-T H.273 的编号）。
    matrix: u32,
    full_range: bool,
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    chroma_width: usize,
    bits: u32,
}

/// 解码一段 AV1 OBU 数据（AVIF 的一个图像项）。
fn decode_frame(obu: &[u8]) -> Result<Frame, TextureError> {
    if obu.is_empty() {
        return Err(err("AVIF 图像项是空的"));
    }
    // SAFETY：见模块文档。所有指针都指向本函数栈上的对象，
    // 生命周期覆盖每一次调用；返回前按 dav1d 的约定逐个释放。
    unsafe {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        dav1d_default_settings(NonNull::new_unchecked(settings.as_mut_ptr()));
        let mut settings = settings.assume_init();
        // 一张贴图一帧，多线程只会多开线程池。
        settings.n_threads = 1;
        settings.max_frame_delay = 1;

        let mut context: Option<Dav1dContext> = None;
        let opened = dav1d_open(NonNull::new(&mut context), NonNull::new(&mut settings));
        if opened.0 < 0 || context.is_none() {
            return Err(err(format!("AV1 解码器打不开（{}）", opened.0)));
        }

        let mut data: Dav1dData = Default::default();
        let buffer = dav1d_data_create(NonNull::new(&mut data), obu.len());
        if buffer.is_null() {
            dav1d_close(NonNull::new(&mut context));
            return Err(err("AV1 数据缓冲分配失败"));
        }
        std::ptr::copy_nonoverlapping(obu.as_ptr(), buffer, obu.len());

        let mut picture: Dav1dPicture = Default::default();
        let mut result = Err(err("AV1 数据里没有解出任何帧"));
        // dav1d 的约定：send 可能只吃掉一部分数据（返回 EAGAIN），
        // 这时先取图再接着送。单帧图像几轮之内一定能结束。
        for _ in 0..64 {
            if data.sz > 0 {
                let sent = dav1d_send_data(context, NonNull::new(&mut data));
                if sent.0 < 0 && sent.0 != -11 {
                    result = Err(err(format!("AV1 数据被解码器拒绝（{}）", sent.0)));
                    break;
                }
            }
            let got = dav1d_get_picture(context, NonNull::new(&mut picture));
            if got.0 == 0 {
                result = copy_frame(&picture);
                dav1d_picture_unref(NonNull::new(&mut picture));
                break;
            }
            if got.0 != -11 {
                result = Err(err(format!("AV1 解码失败（{}）", got.0)));
                break;
            }
            if data.sz == 0 {
                // 数据都送完了还要 EAGAIN：再试几次让解码器把帧吐出来。
                continue;
            }
        }

        if data.sz > 0 {
            dav1d_data_unref(NonNull::new(&mut data));
        }
        dav1d_close(NonNull::new(&mut context));
        result
    }
}

/// 把 dav1d 的输出帧拷成自己的平面。
///
/// # Safety
///
/// `picture` 必须是 `dav1d_get_picture` 刚刚成功返回的帧。
unsafe fn copy_frame(picture: &Dav1dPicture) -> Result<Frame, TextureError> {
    let width = picture.p.w.max(0) as usize;
    let height = picture.p.h.max(0) as usize;
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(err("AVIF 尺寸不在 1..16384 之间"));
    }
    let bits = picture.p.bpc.max(8) as u32;
    let layout = picture.p.layout;
    let (chroma_width, chroma_height) = match layout {
        DAV1D_PIXEL_LAYOUT_I420 => (width.div_ceil(2), height.div_ceil(2)),
        DAV1D_PIXEL_LAYOUT_I422 => (width.div_ceil(2), height),
        DAV1D_PIXEL_LAYOUT_I400 => (0, 0),
        _ => (width, height),
    };
    let (matrix, full_range) = match picture.seq_hdr {
        // SAFETY：帧有效期间序列头指针有效。
        Some(header) => unsafe {
            let header = header.as_ref();
            (header.mtrx as u32, header.color_range != 0)
        },
        None => (1, false),
    };

    let read_plane = |index: usize, w: usize, h: usize, stride: isize| -> Result<Vec<u16>, TextureError> {
        let Some(base) = picture.data[index] else {
            return Err(err("AV1 帧缺少平面数据"));
        };
        let base = base.as_ptr() as *const u8;
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            // SAFETY：dav1d 保证每个平面至少有 `h` 行、每行 `stride` 字节。
            unsafe {
                let line = base.offset(row as isize * stride);
                if bits > 8 {
                    let line = line as *const u16;
                    out.extend((0..w).map(|x| line.add(x).read_unaligned()));
                } else {
                    out.extend((0..w).map(|x| *line.add(x) as u16));
                }
            }
        }
        Ok(out)
    };

    let y = read_plane(0, width, height, picture.stride[0])?;
    let (u, v) = if chroma_width > 0 {
        (
            read_plane(1, chroma_width, chroma_height, picture.stride[1])?,
            read_plane(2, chroma_width, chroma_height, picture.stride[1])?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(Frame {
        width,
        height,
        layout,
        matrix,
        full_range,
        y,
        u,
        v,
        chroma_width,
        bits,
    })
}

/// 矩阵系数 → `(Kr, Kb)`。`None` 表示恒等矩阵（平面本身就是 G / B / R）。
fn coefficients(matrix: u32) -> Option<(f32, f32)> {
    match matrix {
        0 => None,
        // 2 = 未指定：libavif（因而 Chrome / Firefox）按 BT.601 处理。
        2 | 5 | 6 => Some((0.299, 0.114)),
        9 | 10 => Some((0.2627, 0.0593)),
        4 => Some((0.30, 0.11)),
        7 => Some((0.212, 0.087)),
        // 1 = BT.709，其余少见的编号也按它处理。
        _ => Some((0.2126, 0.0722)),
    }
}

/// 把一帧转成 RGBA8。
fn to_rgba(frame: &Frame, alpha: Option<&Frame>, premultiplied: bool) -> Vec<u8> {
    let max = ((1u32 << frame.bits) - 1) as f32;
    let scale = (1u32 << (frame.bits - 8)) as f32;
    let (y_offset, y_range, c_range) = if frame.full_range {
        (0.0, max, max)
    } else {
        (16.0 * scale, 219.0 * scale, 224.0 * scale)
    };
    let half = (1u32 << (frame.bits - 1)) as f32;
    let matrix = coefficients(frame.matrix);
    let (sub_x, sub_y) = match frame.layout {
        DAV1D_PIXEL_LAYOUT_I420 => (1, 1),
        DAV1D_PIXEL_LAYOUT_I422 => (1, 0),
        _ => (0, 0),
    };
    let alpha_max = alpha.map_or(1.0, |a| ((1u32 << a.bits) - 1) as f32);

    let mut out = vec![0u8; frame.width * frame.height * 4];
    for py in 0..frame.height {
        for px in 0..frame.width {
            let luma = (frame.y[py * frame.width + px] as f32 - y_offset) / y_range;
            let (r, g, b) = if frame.chroma_width == 0 {
                (luma, luma, luma)
            } else {
                let ci = (py >> sub_y) * frame.chroma_width + (px >> sub_x);
                let cb = (frame.u[ci] as f32 - half) / c_range;
                let cr = (frame.v[ci] as f32 - half) / c_range;
                match matrix {
                    Some((kr, kb)) => {
                        let kg = 1.0 - kr - kb;
                        let r = luma + 2.0 * (1.0 - kr) * cr;
                        let b = luma + 2.0 * (1.0 - kb) * cb;
                        let g = (luma - kr * r - kb * b) / kg;
                        (r, g, b)
                    }
                    // 恒等矩阵：Y 平面是 G，U 是 B，V 是 R。
                    None => (cr + 0.5, luma, cb + 0.5),
                }
            };
            let a = alpha.map_or(1.0, |a| {
                let index = py.min(a.height - 1) * a.width + px.min(a.width - 1);
                a.y[index] as f32 / alpha_max
            });
            let unpremultiply = |c: f32| if premultiplied && a > 0.0 { c / a } else { c };
            let pixel = &mut out[(py * frame.width + px) * 4..][..4];
            pixel[0] = (unpremultiply(r).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            pixel[1] = (unpremultiply(g).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            pixel[2] = (unpremultiply(b).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            pixel[3] = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    out
}

/// 解码一张 AVIF 成 `(宽, 高, RGBA8)`。
pub(crate) fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), TextureError> {
    let parsed = avif_parse::read_avif(&mut std::io::Cursor::new(bytes))
        .map_err(|e| err(format!("AVIF 容器解析失败：{e}")))?;
    let color = decode_frame(&parsed.primary_item)?;
    let alpha = match parsed.alpha_item.as_ref() {
        Some(item) => match decode_frame(item) {
            Ok(frame) => Some(frame),
            Err(error) => {
                klog::warn!("AVIF 的 alpha 图层解不出来，按不透明处理：{error}");
                None
            }
        },
        None => None,
    };
    let rgba = to_rgba(&color, alpha.as_ref(), parsed.premultiplied_alpha);
    Ok((color.width as u32, color.height as u32, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_the_avif_brand() {
        let mut header = vec![0, 0, 0, 20];
        header.extend_from_slice(b"ftypavif\0\0\0\0mif1");
        assert!(sniff(&header));
        assert!(!sniff(b"\x89PNG\r\n\x1a\n...."));
    }

    #[test]
    fn full_range_grey_stays_grey() {
        let frame = Frame {
            width: 1,
            height: 1,
            layout: 3,
            matrix: 1,
            full_range: true,
            y: vec![128],
            u: vec![128],
            v: vec![128],
            chroma_width: 1,
            bits: 8,
        };
        let rgba = to_rgba(&frame, None, false);
        assert_eq!(rgba, [128, 128, 128, 255]);
    }

    /// 从 three.js 的 `forest_house.glb` 里掏一张 AVIF 出来真解一次。
    #[test]
    fn decodes_a_real_avif_from_forest_house() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/threejs/models/gltf/AVIFTest/forest_house.glb");
        let Ok(glb) = std::fs::read(path) else { return };
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let json: serde_json::Value = serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
        let bin = &glb[20 + json_len + 8..];
        let view = &json["bufferViews"][json["images"][0]["bufferView"].as_u64().unwrap() as usize];
        let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let length = view["byteLength"].as_u64().unwrap() as usize;
        let (width, height, rgba) = decode(&bin[offset..offset + length]).unwrap();
        assert!(width > 0 && height > 0);
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        // 不是一张纯黑 / 纯白：解码错了的 AV1 最常见的样子是一片灰或一片黑。
        let distinct = rgba.chunks(4).map(|p| p[0]).collect::<std::collections::HashSet<_>>();
        assert!(distinct.len() > 16, "只有 {} 种红色值", distinct.len());
    }
}
