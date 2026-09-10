//! Portable CPU fallback for PVRTC and KTX1. Decodes level zero to RGBA8.
use crate::{Texture, TextureError};
fn err(message: &str) -> TextureError {
    TextureError(message.into())
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, TextureError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| err("truncated texture header"))?
            .try_into()
            .unwrap(),
    ))
}
pub(crate) fn decode(bytes: &[u8]) -> Option<Result<Texture, TextureError>> {
    if bytes.starts_with(b"\xabKTX 11\xbb\r\n\x1a\n") {
        return Some(ktx(bytes));
    }
    if bytes.starts_with(b"PVR\x03") || bytes.get(44..48) == Some(b"PVR!") {
        return Some(pvr(bytes));
    }
    None
}
type Decoder = fn(&[u8], usize, usize, &mut [u32]) -> Result<(), &'static str>;
fn pixels(bytes: &[u8], width: u32, height: u32, decode: Decoder) -> Result<Texture, TextureError> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(err("compressed texture dimensions outside 1..8192"));
    }
    let mut pixels = vec![0; width as usize * height as usize];
    decode(bytes, width as usize, height as usize, &mut pixels).map_err(err)?;
    // texture2ddecoder outputs little-endian BGRA words.
    let data = pixels
        .into_iter()
        .flat_map(|p| {
            let [b, g, r, a] = p.to_le_bytes();
            [r, g, b, a]
        })
        .collect();
    Ok(Texture::new(width, height, data))
}
fn pvr(bytes: &[u8]) -> Result<Texture, TextureError> {
    let (width, height, offset, two, alpha) = if bytes.starts_with(b"PVR\x03") {
        let format = u32_at(bytes, 8)?;
        if u32_at(bytes, 12)? != 0
            || format > 3
            || u32_at(bytes, 32)? != 1
            || u32_at(bytes, 36)? != 1
            || u32_at(bytes, 40)? != 1
        {
            return Err(err("PVR requires a single 2D PVRTC1 image"));
        }
        (
            u32_at(bytes, 28)?,
            u32_at(bytes, 24)?,
            52usize
                .checked_add(u32_at(bytes, 48)? as usize)
                .ok_or_else(|| err("PVR offset overflow"))?,
            format < 2,
            format % 2 == 1,
        )
    } else {
        let format = u32_at(bytes, 16)? & 255;
        if ![24, 25].contains(&format) || u32_at(bytes, 48)? > 1 {
            return Err(err("unsupported PVR v2 type"));
        }
        (
            u32_at(bytes, 8)?,
            u32_at(bytes, 4)?,
            u32_at(bytes, 0)? as usize,
            format == 24,
            u32_at(bytes, 40)? != 0,
        )
    };
    if !width.is_power_of_two() || !height.is_power_of_two() {
        return Err(err("PVRTC dimensions must be powers of two"));
    }
    let minimum = (width.max(if two { 16 } else { 8 }) as usize) * (height.max(8) as usize)
        / if two { 4 } else { 2 };
    let data = bytes
        .get(offset..)
        .ok_or_else(|| err("invalid PVR payload offset"))?;
    if data.len() < minimum {
        return Err(err("truncated PVRTC payload"));
    }
    let mut result = pixels(
        data,
        width,
        height,
        if two {
            texture2ddecoder::decode_pvrtc_2bpp
        } else {
            texture2ddecoder::decode_pvrtc_4bpp
        },
    )?;
    if !alpha {
        for pixel in std::sync::Arc::make_mut(&mut result.data).chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Ok(result)
}
fn ktx(bytes: &[u8]) -> Result<Texture, TextureError> {
    if u32_at(bytes, 12)? != 0x04030201 {
        return Err(err("big-endian KTX is not supported"));
    }
    if u32_at(bytes, 20)? != 1
        || u32_at(bytes, 44)? > 1
        || u32_at(bytes, 48)? != 0
        || u32_at(bytes, 52)? != 1
    {
        return Err(err("KTX requires a 2D, non-array image"));
    }
    let width = u32_at(bytes, 36)?;
    let height = u32_at(bytes, 40)?;
    let offset = 64usize
        .checked_add(u32_at(bytes, 60)? as usize)
        .ok_or_else(|| err("KTX offset overflow"))?;
    let size = u32_at(bytes, offset)? as usize;
    let start = offset
        .checked_add(4)
        .ok_or_else(|| err("KTX offset overflow"))?;
    let data = bytes
        .get(
            start
                ..start
                    .checked_add(size)
                    .ok_or_else(|| err("KTX size overflow"))?,
        )
        .ok_or_else(|| err("truncated KTX level"))?;
    let decoder: Decoder = match u32_at(bytes, 28)? {
        0x83F0 | 0x83F1 => texture2ddecoder::decode_bc1,
        0x83F2 => texture2ddecoder::decode_bc2,
        0x83F3 => texture2ddecoder::decode_bc3,
        0x8D64 => texture2ddecoder::decode_etc1,
        0x9274 => texture2ddecoder::decode_etc2_rgb,
        0x9278 => texture2ddecoder::decode_etc2_rgba8,
        0x8C00 | 0x8C02 => texture2ddecoder::decode_pvrtc_4bpp,
        0x8C01 | 0x8C03 => texture2ddecoder::decode_pvrtc_2bpp,
        0x93B0 => texture2ddecoder::decode_astc_4_4,
        0x93B7 => texture2ddecoder::decode_astc_8_8,
        _ => return Err(err("unsupported KTX1 pixel format")),
    };
    pixels(data, width, height, decoder)
}
