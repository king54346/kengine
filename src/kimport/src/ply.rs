//! Stanford PLY。ASCII 与二进制（大小端都行）。
//!
//! # 支持
//!
//! `element vertex` 里的 `x/y/z`、`nx/ny/nz`、`red/green/blue/alpha`、
//! 以及三套写法的纹理坐标（`s/t`、`u/v`、`texture_u/texture_v`）；
//! `element face` 的列表属性（`vertex_indices` 或 `vertex_index`），
//! 多边形按扇形三角化。其余 element 按属性宽度**跳过**而不是报错——
//! 扫描仪导出的 PLY 里常有 `element camera` 之类的附加块。
//!
//! # 不支持
//!
//! `format ascii` 之外的注释语法扩展、双精度属性会被降到 f32、
//! 以及点云式 PLY（没有 face 元素）——那种应当交给 [`crate::pcd`]
//! 那条点云路径，这里只产出三角形网格。

use crate::{bad, limits, loader, single_mesh_model};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::Material;
use kmath::Vec4;
use kmesh::{Mesh, Vertex};
use std::{path::PathBuf, sync::Arc};

loader! {
    /// 读 `.ply`。
    PlyLoader -> Model : ["ply"] = MODEL_TYPE_UUID, parse
}

/// 解析 PLY。不需要附属文件。
pub async fn parse(
    bytes: Vec<u8>,
    path: PathBuf,
    _io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "PLY".into());
    let header = Header::parse(&bytes)?;
    let (vertices, indices) = match header.format {
        Format::Ascii => read_ascii(&bytes, &header)?,
        Format::Binary { big_endian } => read_binary(&bytes, &header, big_endian)?,
    };
    if indices.is_empty() {
        return Err(bad("PLY 里没有面"));
    }

    let mut mesh = Mesh::new(vertices, indices);
    if !header.has_normals() {
        mesh.recompute_normals();
    }
    if header.has_uvs() {
        mesh.recompute_tangents();
    }
    let material = Material::standard()
        .with_name(&name)
        .with_base_color(Vec4::ONE)
        .with_metallic(0.0)
        .with_roughness(0.6);
    Ok(single_mesh_model(&name, mesh, material))
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Ascii,
    Binary { big_endian: bool },
}

#[derive(Clone, Copy, PartialEq)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// 读一个标量并统一成 f64。整型属性（颜色）保持原值，归一化由调用方做。
    fn read(self, bytes: &[u8], big_endian: bool) -> f64 {
        macro_rules! number {
            ($ty:ty, $n:literal) => {{
                let raw: [u8; $n] = bytes[..$n].try_into().unwrap();
                if big_endian {
                    <$ty>::from_be_bytes(raw) as f64
                } else {
                    <$ty>::from_le_bytes(raw) as f64
                }
            }};
        }
        match self {
            Self::I8 => bytes[0] as i8 as f64,
            Self::U8 => bytes[0] as f64,
            Self::I16 => number!(i16, 2),
            Self::U16 => number!(u16, 2),
            Self::I32 => number!(i32, 4),
            Self::U32 => number!(u32, 4),
            Self::F32 => number!(f32, 4),
            Self::F64 => number!(f64, 8),
        }
    }
}

struct Property {
    name: String,
    scalar: Scalar,
    /// 列表属性的计数类型。`None` 表示是个普通标量。
    count: Option<Scalar>,
}

struct Element {
    name: String,
    count: usize,
    properties: Vec<Property>,
}

struct Header {
    format: Format,
    elements: Vec<Element>,
    body: usize,
}

impl Header {
    fn parse(bytes: &[u8]) -> Result<Self, LoadError> {
        if !bytes.starts_with(b"ply") {
            return Err(bad("不是 PLY 文件"));
        }
        // 头部一定是 ASCII，正文可能是二进制，所以只按字节找结束标记。
        let end = bytes
            .windows(10)
            .position(|w| w == b"end_header")
            .ok_or_else(|| bad("PLY 头部没有 end_header"))?;
        let mut body = end + 10;
        while matches!(bytes.get(body), Some(b'\r') | Some(b'\n')) {
            body += 1;
        }
        let text = String::from_utf8_lossy(&bytes[..end]);

        let mut format = None;
        let mut elements: Vec<Element> = Vec::new();
        for line in text.lines() {
            let mut tokens = line.split_whitespace();
            match tokens.next() {
                Some("format") => {
                    format = Some(match tokens.next() {
                        Some("ascii") => Format::Ascii,
                        Some("binary_little_endian") => Format::Binary { big_endian: false },
                        Some("binary_big_endian") => Format::Binary { big_endian: true },
                        other => return Err(bad(format!("未知的 PLY 格式 {other:?}"))),
                    });
                }
                Some("element") => {
                    let name = tokens.next().unwrap_or("").to_string();
                    let count: usize = tokens
                        .next()
                        .and_then(|t| t.parse().ok())
                        .ok_or_else(|| bad("element 少了数量"))?;
                    if count > limits::VERTICES {
                        return Err(bad("PLY 元素数量超过上限"));
                    }
                    elements.push(Element {
                        name,
                        count,
                        properties: Vec::new(),
                    });
                }
                Some("property") => {
                    let element = elements
                        .last_mut()
                        .ok_or_else(|| bad("property 出现在 element 之前"))?;
                    let first = tokens.next().unwrap_or("");
                    if first == "list" {
                        let count = Scalar::parse(tokens.next().unwrap_or(""))
                            .ok_or_else(|| bad("列表属性的计数类型未知"))?;
                        let scalar = Scalar::parse(tokens.next().unwrap_or(""))
                            .ok_or_else(|| bad("列表属性的元素类型未知"))?;
                        element.properties.push(Property {
                            name: tokens.next().unwrap_or("").to_string(),
                            scalar,
                            count: Some(count),
                        });
                    } else {
                        let scalar =
                            Scalar::parse(first).ok_or_else(|| bad(format!("未知属性类型 {first}")))?;
                        element.properties.push(Property {
                            name: tokens.next().unwrap_or("").to_string(),
                            scalar,
                            count: None,
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(Self {
            format: format.ok_or_else(|| bad("PLY 头部没有 format 行"))?,
            elements,
            body,
        })
    }

    fn vertex(&self) -> Option<&Element> {
        self.elements.iter().find(|e| e.name == "vertex")
    }

    fn has_normals(&self) -> bool {
        self.vertex()
            .is_some_and(|e| e.properties.iter().any(|p| p.name == "nx"))
    }

    fn has_uvs(&self) -> bool {
        self.vertex().is_some_and(|e| {
            e.properties
                .iter()
                .any(|p| matches!(p.name.as_str(), "s" | "u" | "texture_u"))
        })
    }
}

/// 把一个顶点元素的属性值装进 [`Vertex`]。
fn assemble(properties: &[Property], values: &[f64]) -> Vertex {
    let mut vertex = Vertex::default();
    let mut has_colour = false;
    for (property, &value) in properties.iter().zip(values) {
        let float = value as f32;
        match property.name.as_str() {
            "x" => vertex.position[0] = float,
            "y" => vertex.position[1] = float,
            "z" => vertex.position[2] = float,
            "nx" => vertex.normal[0] = float,
            "ny" => vertex.normal[1] = float,
            "nz" => vertex.normal[2] = float,
            "s" | "u" | "texture_u" => vertex.uv[0] = float,
            // PLY 的 v 轴和图片行号方向相反，和 OBJ 同理。
            "t" | "v" | "texture_v" => vertex.uv[1] = 1.0 - float,
            "red" | "green" | "blue" => {
                has_colour = true;
                // 整型颜色是 0..255，浮点颜色已经是 0..1。
                let normalised = if property.scalar == Scalar::F32 || property.scalar == Scalar::F64
                {
                    float
                } else {
                    float / 255.0
                };
                let channel = match property.name.as_str() {
                    "red" => 0,
                    "green" => 1,
                    _ => 2,
                };
                vertex.color[channel] = normalised;
            }
            _ => {}
        }
    }
    if !has_colour {
        vertex.color = [1.0; 3];
    }
    vertex
}

/// 多边形扇形三角化，顺带把索引范围校验掉。
fn push_face(indices: &mut Vec<u32>, corners: &[u32], vertex_count: usize) -> Result<(), LoadError> {
    if corners.iter().any(|&i| i as usize >= vertex_count) {
        return Err(bad("PLY 的面索引越界"));
    }
    for i in 1..corners.len().saturating_sub(1) {
        indices.extend_from_slice(&[corners[0], corners[i], corners[i + 1]]);
    }
    Ok(())
}

fn read_ascii(bytes: &[u8], header: &Header) -> Result<(Vec<Vertex>, Vec<u32>), LoadError> {
    let text = String::from_utf8_lossy(&bytes[header.body..]);
    let mut numbers = text.split_whitespace();
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut next = || -> Result<f64, LoadError> {
        numbers
            .next()
            .ok_or_else(|| bad("PLY 正文提前结束"))?
            .parse()
            .map_err(|_| bad("PLY 正文里有非数字"))
    };

    for element in &header.elements {
        for _ in 0..element.count {
            let mut scalars = Vec::with_capacity(element.properties.len());
            let mut corners = Vec::new();
            for property in &element.properties {
                if property.count.is_some() {
                    let count = next()? as usize;
                    if count > 1024 {
                        return Err(bad("PLY 的多边形边数过多"));
                    }
                    corners.clear();
                    for _ in 0..count {
                        corners.push(next()? as u32);
                    }
                } else {
                    scalars.push(next()?);
                }
            }
            match element.name.as_str() {
                "vertex" => vertices.push(assemble(&element.properties, &scalars)),
                "face" => push_face(&mut indices, &corners, vertices.len())?,
                _ => {}
            }
        }
    }
    Ok((vertices, indices))
}

fn read_binary(
    bytes: &[u8],
    header: &Header,
    big_endian: bool,
) -> Result<(Vec<Vertex>, Vec<u32>), LoadError> {
    let mut cursor = header.body;
    let mut take = |size: usize| -> Result<&[u8], LoadError> {
        let slice = bytes
            .get(cursor..cursor + size)
            .ok_or_else(|| bad("PLY 正文提前结束"))?;
        cursor += size;
        Ok(slice)
    };

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for element in &header.elements {
        if element.name == "vertex" {
            vertices.reserve(element.count);
        }
        for _ in 0..element.count {
            let mut scalars = Vec::with_capacity(element.properties.len());
            let mut corners = Vec::new();
            for property in &element.properties {
                if let Some(count_type) = property.count {
                    let count = count_type.read(take(count_type.size())?, big_endian) as usize;
                    if count > 1024 {
                        return Err(bad("PLY 的多边形边数过多"));
                    }
                    corners.clear();
                    for _ in 0..count {
                        corners.push(
                            property.scalar.read(take(property.scalar.size())?, big_endian) as u32,
                        );
                    }
                } else {
                    scalars.push(property.scalar.read(take(property.scalar.size())?, big_endian));
                }
            }
            match element.name.as_str() {
                "vertex" => vertices.push(assemble(&element.properties, &scalars)),
                "face" => push_face(&mut indices, &corners, vertices.len())?,
                _ => {}
            }
        }
    }
    Ok((vertices, indices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;

    fn load(bytes: Vec<u8>) -> Result<Model, LoadError> {
        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        ktask::block_on(parse(bytes, PathBuf::from("t.ply"), io))
    }

    const ASCII: &str = "ply\nformat ascii 1.0\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 255 0 0\n1 0 0 0 255 0\n1 1 0 0 0 255\n0 1 0 255 255 255\n4 0 1 2 3\n";

    #[test]
    fn reads_ascii_with_colours_and_a_quad() {
        let model = load(ASCII.as_bytes().to_vec()).unwrap();
        assert_eq!(model.triangle_count(), 2, "四边形应当三角化成两个");
        assert_eq!(model.mesh(0).unwrap().vertices()[0].color, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn reads_binary_little_endian() {
        let mut bytes = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n".to_vec();
        for position in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for value in position {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        bytes.push(3);
        for index in [0u32, 1, 2] {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        assert_eq!(load(bytes).unwrap().triangle_count(), 1);
    }

    #[test]
    fn out_of_range_face_indices_are_rejected() {
        let broken = ASCII.replace("4 0 1 2 3", "4 0 1 2 9");
        assert!(load(broken.into_bytes()).is_err());
    }

    #[test]
    fn a_truncated_body_is_rejected() {
        let broken = ASCII.replace("0 1 0 255 255 255\n4 0 1 2 3\n", "");
        assert!(load(broken.into_bytes()).is_err());
    }
}
