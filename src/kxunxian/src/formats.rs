//! 三种二进制格式：网格 `.pmf`、骨架 `.psf`、动作 `.paf`。
//!
//! 布局照 XunxianDpkViewer 的 `PmfParser` / `PsfParser` / `PafParser`（反编译出来、已经验证过的那份）。
//! 坐标是 D3D 的左手系、Y 朝上；这里原样读出来，换手系在 [`crate::character`] 里统一做。

use std::collections::HashMap;
use std::fmt;

use kmath::{Quat, Vec2, Vec3};

/// 格式错误。
#[derive(Debug, Clone, PartialEq)]
pub struct FormatError(pub String);

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FormatError {}

fn error<T>(message: impl Into<String>) -> Result<T, FormatError> {
    Err(FormatError(message.into()))
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn at(data: &'a [u8], offset: usize) -> Self {
        Self { data, offset }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|&end| end <= self.data.len());
        let Some(end) = end else {
            return error("数据不完整");
        };
        let slice = &self.data[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, FormatError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> Result<i16, FormatError> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, FormatError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn vec3(&mut self) -> Result<Vec3, FormatError> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
}

fn normalized(q: Quat) -> Quat {
    if q.length_squared() > 1e-6 {
        q.normalize()
    } else {
        Quat::IDENTITY
    }
}

// ---------------------------------------------------------------------------------------------
// .pmf
// ---------------------------------------------------------------------------------------------

/// 一个网格。
#[derive(Debug, Clone, Default)]
pub struct PmfMesh {
    pub positions: Vec<Vec3>,
    /// 文件里有法线就读（长度不对的话调用方自己重算）。
    pub normals: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
    pub indices: Vec<u32>,
    /// 有蒙皮时每顶点四个权重（已归一化）和四个骨骼序号。
    pub weights: Vec<[f32; 4]>,
    pub joints: Vec<[u16; 4]>,
}

impl PmfMesh {
    pub fn is_skinned(&self) -> bool {
        !self.weights.is_empty()
    }
}

/// 解析 `.pmf`。
///
/// 顶点数据是「结构数组」：先是全部顶点的位置，再是（有蒙皮时）全部顶点的 3 个权重 + 4 个 `u16` 骨骼号，
/// 再是法线，最后是 UV 组；索引是 `u16` 三元组，放在文件最末尾。
///
/// **每顶点有多少字节**才是判断布局的依据，`flags` 的 0x10 位不可靠：武器和法宝的 `flags` 是 0x11，
/// 但每顶点只有 24 字节（位置 + 法线），并没有蒙皮。按 0x10 去读的话，法线会被当成骨骼号。
/// 实测的几种：44 = 位置 + 权重 + 骨骼号 + 法线；48 = 再加 4 字节顶点色；24 = 位置 + 法线；28 = 再加颜色。
pub fn parse_pmf(data: &[u8]) -> Result<PmfMesh, FormatError> {
    if data.len() < 48 || &data[4..8] != b"PMF\0" {
        return error("不是寻仙 PMF 网格");
    }
    let mut header = Reader::at(data, 0);
    let header_size = header.u32()?;
    header.offset = 12;
    let _flags = header.u32()?;
    let vertex_count = header.u32()? as usize;
    let triangle_count = header.u32()? as usize;
    let uv_sets = header.u32()? as usize;
    if header_size != 28 || vertex_count == 0 || vertex_count > 5_000_000 {
        return error("PMF 头部异常");
    }
    let base = 48;
    let index_bytes = triangle_count * 6;
    let uv_bytes = vertex_count * uv_sets * 8;
    let Some(remainder) = data.len().checked_sub(base + index_bytes + uv_bytes) else {
        return error("PMF 数据不完整");
    };
    if remainder % vertex_count != 0 || remainder / vertex_count < 12 {
        return error("PMF 顶点数据大小对不上");
    }
    let stride = remainder / vertex_count;

    let mut reader = Reader::at(data, base);
    let mut mesh = PmfMesh {
        positions: (0..vertex_count)
            .map(|_| reader.vec3())
            .collect::<Result<_, _>>()?,
        ..Default::default()
    };
    if stride >= 44 {
        let weights_at = reader.offset;
        let joints_at = weights_at + vertex_count * 12;
        let mut weights = Reader::at(data, weights_at);
        let mut joints = Reader::at(data, joints_at);
        for _ in 0..vertex_count {
            let w0 = weights.f32()?.max(0.0);
            let w1 = weights.f32()?.max(0.0);
            let w2 = weights.f32()?.max(0.0);
            // 第四个权重不存，`1 - 前三个`。
            let w3 = (1.0 - w0 - w1 - w2).max(0.0);
            let sum = w0 + w1 + w2 + w3;
            mesh.weights.push(if sum > 1e-6 {
                [w0 / sum, w1 / sum, w2 / sum, w3 / sum]
            } else {
                [0.0; 4]
            });
            mesh.joints
                .push([joints.u16()?, joints.u16()?, joints.u16()?, joints.u16()?]);
        }
        reader.offset = joints_at + vertex_count * 8;
    }
    let consumed = reader.offset - base;
    if remainder - consumed >= vertex_count * 12 {
        mesh.normals = (0..vertex_count)
            .map(|_| reader.vec3())
            .collect::<Result<_, _>>()?;
    }
    if uv_sets > 0 {
        let mut uv = Reader::at(data, base + remainder);
        for _ in 0..vertex_count {
            mesh.uvs.push(Vec2::new(uv.f32()?, uv.f32()?));
        }
    }
    let mut indices = Reader::at(data, data.len() - index_bytes);
    for _ in 0..triangle_count * 3 {
        let index = indices.u16()? as u32;
        if index as usize >= vertex_count {
            return error("PMF 索引超出顶点范围");
        }
        mesh.indices.push(index);
    }
    Ok(mesh)
}

// ---------------------------------------------------------------------------------------------
// .psf
// ---------------------------------------------------------------------------------------------

/// 一根骨骼。两组 TR：绑定姿态的**局部**变换（相对父骨），和逆绑定（模型空间 → 骨骼空间）。
#[derive(Debug, Clone, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: i32,
    pub bind_translation: Vec3,
    pub bind_rotation: Quat,
    pub inverse_translation: Vec3,
    pub inverse_rotation: Quat,
}

/// 解析 `.psf`。每根骨骼：名字长度 + UTF-16 名字 + 父骨号 + 20 个 float + 4 字节保留 + 子骨表。
/// 四元数按 `(x, y, z, w)` 存。
pub fn parse_psf(data: &[u8]) -> Result<Vec<Bone>, FormatError> {
    if data.len() < 20 || &data[..4] != b"PSF\0" {
        return error("不是寻仙 PSF 骨架");
    }
    let mut reader = Reader::at(data, 16);
    let count = reader.u32()? as usize;
    if count == 0 || count > 100_000 {
        return error("PSF 骨骼数量异常");
    }
    let mut bones = Vec::with_capacity(count);
    for _ in 0..count {
        let length = reader.u32()? as usize;
        let units: Vec<u16> = reader
            .take(length * 2)?
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let name = String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string();
        let parent = reader.i32()?;
        let mut fixed = [0.0f32; 20];
        for value in &mut fixed {
            *value = reader.f32()?;
        }
        reader.take(4)?;
        let children = reader.u32()? as usize;
        if children > count {
            return error("PSF 子骨骼数量异常");
        }
        reader.take(children * 4)?;
        bones.push(Bone {
            name,
            parent,
            bind_translation: Vec3::new(fixed[0], fixed[1], fixed[2]),
            bind_rotation: normalized(Quat::from_xyzw(fixed[3], fixed[4], fixed[5], fixed[6])),
            inverse_translation: Vec3::new(fixed[7], fixed[8], fixed[9]),
            inverse_rotation: normalized(Quat::from_xyzw(
                fixed[10], fixed[11], fixed[12], fixed[13],
            )),
        });
    }
    Ok(bones)
}

// ---------------------------------------------------------------------------------------------
// .paf
// ---------------------------------------------------------------------------------------------

/// 一根骨骼的关键帧：按 `sample_rate` 均匀排列。平移可以一帧都没有（用绑定姿态）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoneTrack {
    pub rotations: Vec<Quat>,
    pub translations: Vec<Vec3>,
}

/// 一段动作。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PafAnimation {
    pub sample_rate: u32,
    pub duration: f32,
    /// 骨骼号 → 关键帧。
    pub tracks: HashMap<i32, BoneTrack>,
}

/// 解析 `.paf`。版本 101 的四元数是 `i16 / 32767`，100 是 `f32`；分量顺序 `(x, y, z, w)`。
pub fn parse_paf(data: &[u8]) -> Result<PafAnimation, FormatError> {
    if data.len() < 20 || &data[..4] != b"PAF\0" {
        return error("不是寻仙 PAF 动作");
    }
    let mut reader = Reader::at(data, 4);
    let version = reader.u32()?;
    let sample_rate = reader.u32()?;
    let duration = reader.f32()?;
    let track_count = reader.u32()? as usize;
    if !(100..=101).contains(&version)
        || sample_rate == 0
        || sample_rate > 1000
        || !duration.is_finite()
        || duration < 0.0
        || track_count > 100_000
    {
        return error("PAF 头部异常");
    }
    let mut animation = PafAnimation {
        sample_rate,
        duration,
        tracks: HashMap::new(),
    };
    for _ in 0..track_count {
        let bone = reader.i32()?;
        let rotation_count = reader.u32()? as usize;
        if rotation_count > 10_000_000 {
            return error("PAF 关键帧数量异常");
        }
        let mut track = BoneTrack::default();
        for _ in 0..rotation_count {
            let q = if version == 101 {
                let mut c = [0.0f32; 4];
                for value in &mut c {
                    *value = reader.i16()? as f32 / 32767.0;
                }
                Quat::from_array(c)
            } else {
                Quat::from_xyzw(reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?)
            };
            track.rotations.push(normalized(q));
        }
        let translation_count = reader.u32()? as usize;
        if translation_count > 10_000_000 {
            return error("PAF 关键帧数量异常");
        }
        for _ in 0..translation_count {
            track.translations.push(reader.vec3()?);
        }
        animation.tracks.insert(bone, track);
    }
    Ok(animation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pmf(vertex_count: u32, triangles: &[u16], stride_extra: &[u8], uv_sets: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&28u32.to_le_bytes());
        data.extend_from_slice(b"PMF\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0x11u32.to_le_bytes());
        data.extend_from_slice(&vertex_count.to_le_bytes());
        data.extend_from_slice(&((triangles.len() / 3) as u32).to_le_bytes());
        data.extend_from_slice(&uv_sets.to_le_bytes());
        data.resize(48, 0);
        for i in 0..vertex_count {
            for c in [i as f32, 0.0, 0.0] {
                data.extend_from_slice(&c.to_le_bytes());
            }
        }
        data.extend_from_slice(stride_extra);
        for i in 0..vertex_count * uv_sets {
            data.extend_from_slice(&(i as f32).to_le_bytes());
            data.extend_from_slice(&0.5f32.to_le_bytes());
        }
        for index in triangles {
            data.extend_from_slice(&index.to_le_bytes());
        }
        data
    }

    #[test]
    fn rigid_mesh_with_skin_flag_is_not_skinned() {
        // flags 0x11 + 每顶点 24 字节：位置 + 法线，不是蒙皮。
        let mut normals = Vec::new();
        for _ in 0..3 {
            for c in [0.0f32, 1.0, 0.0] {
                normals.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mesh = parse_pmf(&pmf(3, &[0, 1, 2], &normals, 1)).unwrap();
        assert!(!mesh.is_skinned());
        assert_eq!(mesh.normals, vec![Vec3::Y; 3]);
        assert_eq!(mesh.indices, [0, 1, 2]);
        assert_eq!(mesh.uvs[2], Vec2::new(2.0, 0.5));
    }

    #[test]
    fn skinned_mesh_derives_fourth_weight() {
        let mut extra = Vec::new();
        for _ in 0..3 {
            for w in [0.5f32, 0.25, 0.0] {
                extra.extend_from_slice(&w.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for j in [7u16, 8, 0, 9] {
                extra.extend_from_slice(&j.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for c in [0.0f32, 0.0, 1.0] {
                extra.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mesh = parse_pmf(&pmf(3, &[0, 1, 2], &extra, 1)).unwrap();
        assert_eq!(mesh.weights[0], [0.5, 0.25, 0.0, 0.25]);
        assert_eq!(mesh.joints[1], [7, 8, 0, 9]);
        assert_eq!(mesh.normals[2], Vec3::Z);
    }

    #[test]
    fn rejects_out_of_range_index() {
        let normals = vec![0u8; 3 * 12];
        assert!(parse_pmf(&pmf(3, &[0, 1, 3], &normals, 1)).is_err());
    }

    #[test]
    fn reads_packed_quaternion_animation() {
        let mut data = b"PAF\0".to_vec();
        for v in [101u32, 30] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.extend_from_slice(&1.0f32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&5i32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        for c in [0i16, 0, 0, 32767] {
            data.extend_from_slice(&c.to_le_bytes());
        }
        data.extend_from_slice(&0u32.to_le_bytes());
        let animation = parse_paf(&data).unwrap();
        assert_eq!(animation.sample_rate, 30);
        assert_eq!(animation.tracks[&5].rotations, [Quat::IDENTITY]);
        assert!(animation.tracks[&5].translations.is_empty());
    }
}
