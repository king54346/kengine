//! PCD（Point Cloud Data，PCL 的原生格式）。
//!
//! # 支持
//!
//! `DATA ascii` / `DATA binary` / `DATA binary_compressed` 三种正文，
//! 字段里认 `x` `y` `z`、`rgb` / `rgba`（打包成一个 32 位字的 BGRA）、
//! `normal_x/y/z`、`intensity`。字段顺序、大小、类型全部按头部走，
//! 不认识的字段按宽度跳过。
//!
//! # `binary_compressed` 是 SoA
//!
//! 这是这个格式最容易读错的地方：压缩正文解开之后**不是**一个点接一个点，
//! 而是「所有点的 x、所有点的 y、所有点的 z……」。按 AoS 去读会得到一团
//! 看起来像噪声的东西，而且不会报任何错——所以下面单独有一条测试盯着它。
//!
//! 压缩算法是 LZF（不是 zlib），PCL 自带的那一版，实现见 [`lzf_decompress`]。

use crate::{bad, limits};
use kasset::LoadError;
use kmath::Vec3;
use kmesh::Mesh;

/// 一朵点云：位置与颜色，可选法线。
#[derive(Debug, Clone, Default)]
pub struct PointCloud {
    /// 每个点的位置。
    pub positions: Vec<Vec3>,
    /// 每个点的颜色，文件没给颜色时全是白色。
    pub colors: Vec<Vec3>,
    /// 文件里有没有真正的颜色字段。没有时调用方通常会按高度或强度上色。
    pub has_color: bool,
}

impl PointCloud {
    /// 点数。
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// 包围盒，摆相机用。
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.positions.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(min, max), &p| (min.min(p), max.max(p)),
        )
    }

    /// 把点云变成一份可渲染的几何：每个点一个正方形面片。
    ///
    /// 面片本身是**退化的**——四个顶点都在点的位置上，靠
    /// [`kpbr::points`] 那个顶点钩子在顶点阶段按相机朝向张开。
    /// 详见 [`Mesh::point_sprites`]。
    pub fn to_mesh(&self) -> Mesh {
        Mesh::point_sprites(&self.positions, &self.colors)
    }
}

/// 解析 PCD。
pub fn parse(bytes: &[u8]) -> Result<PointCloud, LoadError> {
    let header = Header::parse(bytes)?;
    let mut cloud = PointCloud {
        positions: Vec::with_capacity(header.points),
        colors: Vec::with_capacity(header.points),
        has_color: header.fields.iter().any(|f| f.is_color()),
    };

    match header.data {
        Data::Ascii => read_ascii(bytes, &header, &mut cloud)?,
        Data::Binary => read_binary(&bytes[header.body..], &header, &mut cloud)?,
        Data::BinaryCompressed => {
            let packed = bytes
                .get(header.body..header.body + 8)
                .ok_or_else(|| bad("PCD 压缩正文缺少长度头"))?;
            let compressed = u32::from_le_bytes(packed[0..4].try_into().unwrap()) as usize;
            let plain = u32::from_le_bytes(packed[4..8].try_into().unwrap()) as usize;
            if plain > 1 << 30 {
                return Err(bad("PCD 解压后的体积超过上限"));
            }
            let payload = bytes
                .get(header.body + 8..header.body + 8 + compressed)
                .ok_or_else(|| bad("PCD 压缩正文被截断"))?;
            let decoded = lzf_decompress(payload, plain)?;
            read_columns(&decoded, &header, &mut cloud)?;
        }
    }
    if cloud.positions.is_empty() {
        return Err(bad("PCD 里没有点"));
    }
    Ok(cloud)
}

#[derive(Clone, Copy, PartialEq)]
enum Data {
    Ascii,
    Binary,
    BinaryCompressed,
}

struct Field {
    name: String,
    size: usize,
    kind: u8,
    count: usize,
}

impl Field {
    fn is_color(&self) -> bool {
        self.name == "rgb" || self.name == "rgba"
    }

    fn bytes(&self) -> usize {
        self.size * self.count
    }

    /// 按字段类型读一个值。`U`（无符号）、`I`（有符号）、`F`（浮点）。
    fn read(&self, bytes: &[u8]) -> f32 {
        match (self.kind, self.size) {
            (b'F', 4) => f32::from_le_bytes(bytes[..4].try_into().unwrap()),
            (b'F', 8) => f64::from_le_bytes(bytes[..8].try_into().unwrap()) as f32,
            (b'U', 1) => bytes[0] as f32,
            (b'U', 2) => u16::from_le_bytes(bytes[..2].try_into().unwrap()) as f32,
            (b'U', 4) => u32::from_le_bytes(bytes[..4].try_into().unwrap()) as f32,
            (b'I', 1) => bytes[0] as i8 as f32,
            (b'I', 2) => i16::from_le_bytes(bytes[..2].try_into().unwrap()) as f32,
            (b'I', 4) => i32::from_le_bytes(bytes[..4].try_into().unwrap()) as f32,
            _ => 0.0,
        }
    }

    /// 颜色字段存的是一个 32 位字，无论声明成 F 还是 U 都要按位取。
    fn read_color(&self, bytes: &[u8]) -> Vec3 {
        let word = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        unpack_color(word)
    }
}

struct Header {
    fields: Vec<Field>,
    points: usize,
    data: Data,
    body: usize,
}

impl Header {
    fn parse(bytes: &[u8]) -> Result<Self, LoadError> {
        // 头部一定是 ASCII，正文可能是二进制：只在前若干字节里找 DATA 行。
        let scan = &bytes[..bytes.len().min(4096)];
        let text = String::from_utf8_lossy(scan);
        let mut fields_names: Vec<String> = Vec::new();
        let mut sizes: Vec<usize> = Vec::new();
        let mut kinds: Vec<u8> = Vec::new();
        let mut counts: Vec<usize> = Vec::new();
        let mut points = 0usize;
        let mut width = 0usize;
        let mut height = 1usize;
        let mut data = None;

        for line in text.lines() {
            let line = line.trim();
            let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            match keyword {
                "FIELDS" => fields_names = rest.split_whitespace().map(str::to_string).collect(),
                "SIZE" => sizes = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect(),
                "TYPE" => {
                    kinds = rest
                        .split_whitespace()
                        .map(|t| t.bytes().next().unwrap_or(b'F'))
                        .collect()
                }
                "COUNT" => counts = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect(),
                "WIDTH" => width = rest.trim().parse().unwrap_or(0),
                "HEIGHT" => height = rest.trim().parse().unwrap_or(1),
                "POINTS" => points = rest.trim().parse().unwrap_or(0),
                "DATA" => {
                    data = Some(match rest.trim() {
                        "ascii" => Data::Ascii,
                        "binary" => Data::Binary,
                        "binary_compressed" => Data::BinaryCompressed,
                        other => return Err(bad(format!("未知的 PCD 正文格式 {other}"))),
                    });
                    break;
                }
                _ => {}
            }
        }

        let data = data.ok_or_else(|| bad("PCD 头部没有 DATA 行"))?;
        // 正文起点直接在**原始字节**里找，不靠上面按行累加的长度：
        // `str::lines()` 会把 `\r` 吃掉，CRLF 的文件按行长累加会逐行少算
        // 一个字节，二进制正文于是整体错位——而错位的二进制点云不会报错，
        // 只会画出一团噪声。
        let marker = bytes
            .windows(5)
            .position(|w| w == b"DATA ")
            .ok_or_else(|| bad("PCD 头部没有 DATA 行"))?;
        let body = bytes[marker..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|offset| marker + offset + 1)
            .ok_or_else(|| bad("PCD 的 DATA 行没有换行"))?;
        if points == 0 {
            points = width.saturating_mul(height);
        }
        if points == 0 || points > limits::VERTICES / 4 {
            return Err(bad("PCD 的点数为零或超过上限"));
        }
        if fields_names.is_empty() || sizes.len() != fields_names.len() {
            return Err(bad("PCD 的 FIELDS 与 SIZE 不匹配"));
        }
        let fields = fields_names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Field {
                name,
                size: sizes[index],
                kind: kinds.get(index).copied().unwrap_or(b'F'),
                count: counts.get(index).copied().unwrap_or(1).max(1),
            })
            .collect();
        Ok(Self {
            fields,
            points,
            data,
            body,
        })
    }

    fn stride(&self) -> usize {
        self.fields.iter().map(Field::bytes).sum()
    }
}

fn unpack_color(word: u32) -> Vec3 {
    Vec3::new(
        ((word >> 16) & 0xff) as f32 / 255.0,
        ((word >> 8) & 0xff) as f32 / 255.0,
        (word & 0xff) as f32 / 255.0,
    )
}

fn read_ascii(bytes: &[u8], header: &Header, cloud: &mut PointCloud) -> Result<(), LoadError> {
    let text = String::from_utf8_lossy(&bytes[header.body..]);
    for line in text.lines() {
        let values: Vec<f32> = line
            .split_whitespace()
            .map(|t| t.parse().unwrap_or(f32::NAN))
            .collect();
        if values.len() < header.fields.len() {
            continue;
        }
        let mut position = Vec3::ZERO;
        let mut color = Vec3::ONE;
        let mut cursor = 0;
        for field in &header.fields {
            let value = values[cursor];
            match field.name.as_str() {
                "x" => position.x = value,
                "y" => position.y = value,
                "z" => position.z = value,
                // ASCII 的颜色列写的是这个 32 位字的十进制值。
                "rgb" | "rgba" => color = unpack_color(value as i64 as u32),
                _ => {}
            }
            cursor += field.count;
        }
        if position.is_finite() {
            cloud.positions.push(position);
            cloud.colors.push(color);
        }
    }
    Ok(())
}

fn read_binary(body: &[u8], header: &Header, cloud: &mut PointCloud) -> Result<(), LoadError> {
    let stride = header.stride();
    if stride == 0 {
        return Err(bad("PCD 的字段宽度为零"));
    }
    for index in 0..header.points {
        let Some(record) = body.get(index * stride..(index + 1) * stride) else {
            break;
        };
        let mut position = Vec3::ZERO;
        let mut color = Vec3::ONE;
        let mut cursor = 0;
        for field in &header.fields {
            let slice = &record[cursor..cursor + field.bytes()];
            match field.name.as_str() {
                "x" => position.x = field.read(slice),
                "y" => position.y = field.read(slice),
                "z" => position.z = field.read(slice),
                "rgb" | "rgba" => color = field.read_color(slice),
                _ => {}
            }
            cursor += field.bytes();
        }
        if position.is_finite() {
            cloud.positions.push(position);
            cloud.colors.push(color);
        }
    }
    Ok(())
}

/// `binary_compressed` 的正文是按字段分列存的，见模块文档。
fn read_columns(body: &[u8], header: &Header, cloud: &mut PointCloud) -> Result<(), LoadError> {
    let mut offsets = Vec::with_capacity(header.fields.len());
    let mut cursor = 0usize;
    for field in &header.fields {
        offsets.push(cursor);
        cursor += field.bytes() * header.points;
    }
    if cursor > body.len() {
        return Err(bad("PCD 解压后的正文不足以装下所有字段"));
    }
    for index in 0..header.points {
        let mut position = Vec3::ZERO;
        let mut color = Vec3::ONE;
        for (field, &offset) in header.fields.iter().zip(&offsets) {
            let start = offset + index * field.bytes();
            let slice = &body[start..start + field.bytes()];
            match field.name.as_str() {
                "x" => position.x = field.read(slice),
                "y" => position.y = field.read(slice),
                "z" => position.z = field.read(slice),
                "rgb" | "rgba" => color = field.read_color(slice),
                _ => {}
            }
        }
        if position.is_finite() {
            cloud.positions.push(position);
            cloud.colors.push(color);
        }
    }
    Ok(())
}

/// LZF 解压（PCL 用的那一版）。
///
/// 控制字节 `< 32` 表示接下来是 `ctrl + 1` 个原样字节；否则高三位是长度
/// （为 7 时再读一个字节加上去），低五位与下一个字节拼成回看距离。
/// 回看的拷贝**必须逐字节**——源和目标可以重叠，正是靠重叠来表达
/// 「重复上一段」。
pub fn lzf_decompress(input: &[u8], expected: usize) -> Result<Vec<u8>, LoadError> {
    let mut output = Vec::with_capacity(expected);
    let mut cursor = 0usize;
    while cursor < input.len() {
        let control = input[cursor] as usize;
        cursor += 1;
        if control < 32 {
            let end = cursor + control + 1;
            let literal = input
                .get(cursor..end)
                .ok_or_else(|| bad("LZF 的原样段越界"))?;
            output.extend_from_slice(literal);
            cursor = end;
        } else {
            let mut length = control >> 5;
            if length == 7 {
                length += *input.get(cursor).ok_or_else(|| bad("LZF 长度字节缺失"))? as usize;
                cursor += 1;
            }
            let low = *input.get(cursor).ok_or_else(|| bad("LZF 距离字节缺失"))? as usize;
            cursor += 1;
            let distance = ((control & 0x1f) << 8) + low + 1;
            if distance > output.len() {
                return Err(bad("LZF 的回看距离越过了输出起点"));
            }
            let mut source = output.len() - distance;
            for _ in 0..length + 2 {
                let byte = output[source];
                output.push(byte);
                source += 1;
            }
        }
        if output.len() > expected {
            return Err(bad("LZF 解压结果超过了声明的长度"));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII: &str = "VERSION 0.7\nFIELDS x y z rgb\nSIZE 4 4 4 4\nTYPE F F F U\nCOUNT 1 1 1 1\nWIDTH 2\nHEIGHT 1\nPOINTS 2\nDATA ascii\n1 2 3 16711680\n4 5 6 255\n";

    #[test]
    fn reads_ascii_points_and_packed_colours() {
        let cloud = parse(ASCII.as_bytes()).unwrap();
        assert_eq!(cloud.len(), 2);
        assert_eq!(cloud.positions[1], Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(cloud.colors[0], Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(cloud.colors[1], Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn reads_binary_points() {
        let mut bytes =
            b"FIELDS x y z\nSIZE 4 4 4\nTYPE F F F\nCOUNT 1 1 1\nPOINTS 2\nDATA binary\n".to_vec();
        for value in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let cloud = parse(&bytes).unwrap();
        assert_eq!(cloud.positions[1], Vec3::new(4.0, 5.0, 6.0));
    }

    /// 分列存储读错的话不会报错，只会得到一团噪声——所以要专门盯着它。
    #[test]
    fn compressed_bodies_are_column_major() {
        let points = 2usize;
        let mut plain = Vec::new();
        for column in [[1.0f32, 4.0], [2.0, 5.0], [3.0, 6.0]] {
            for value in column {
                plain.extend_from_slice(&value.to_le_bytes());
            }
        }
        // 全部当原样段压：LZF 允许，正好用来单独验证分列读取。
        let mut compressed = Vec::new();
        for chunk in plain.chunks(32) {
            compressed.push((chunk.len() - 1) as u8);
            compressed.extend_from_slice(chunk);
        }
        let mut bytes = format!(
            "FIELDS x y z\nSIZE 4 4 4\nTYPE F F F\nCOUNT 1 1 1\nPOINTS {points}\nDATA binary_compressed\n"
        )
        .into_bytes();
        bytes.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&compressed);

        let cloud = parse(&bytes).unwrap();
        assert_eq!(cloud.positions[0], Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(cloud.positions[1], Vec3::new(4.0, 5.0, 6.0));
    }

    #[test]
    fn lzf_repeats_overlapping_runs() {
        // 原样写 "ab"，再回看 2 个字节重复 4 个 —— 重叠拷贝要得到 "ababab"。
        let input = [1u8, b'a', b'b', 0x40 | 0x00, 1];
        assert_eq!(lzf_decompress(&input, 6).unwrap(), b"ababab");
    }

    #[test]
    fn lzf_rejects_a_backreference_before_the_start() {
        assert!(lzf_decompress(&[0x40, 9], 4).is_err());
    }

    #[test]
    fn a_header_without_data_is_rejected() {
        assert!(parse(b"FIELDS x y z\nSIZE 4 4 4\n").is_err());
    }
}
