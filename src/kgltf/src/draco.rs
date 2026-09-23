//! Draco 几何压缩的解码：`KHR_draco_mesh_compression` 与独立的 `.drc` 文件共用。
//!
//! 解码本身交给纯 Rust 的 `draco-core`（与 C++ Draco 1.5.7 逐字节一致，
//! EdgeBreaker / 顺序编码 / 各种预测方案都覆盖）。这里只做一件事：
//! 把它的 `Mesh` 摊平成「每点若干个 `f32` 的属性 + 三角形索引」，
//! 供 glTF 前处理和 `kimport` 的 `.drc` 导入器各取所需。

use draco_core::{DataType, DecoderBuffer, GeometryAttributeType, MeshDecoder};

/// 点数上限。外部文件里写坏的头部不该变成几个 GB 的分配。
const MAX_POINTS: usize = 32_000_000;

/// Draco 属性的语义，对应 `GeometryAttributeType`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DracoSemantic {
    /// 位置。
    Position,
    /// 法线。
    Normal,
    /// 颜色。
    Color,
    /// 纹理坐标。
    TexCoord,
    /// 其它（glTF 里的关节号、权重、切线都落在这一类）。
    Generic,
}

/// 解码出来的一个属性，已经按点展开。
#[derive(Debug, Clone)]
pub struct DracoAttribute {
    /// 属性的唯一 id。glTF 扩展里 `attributes` 映射的就是它。
    pub unique_id: u32,
    /// 语义。
    pub semantic: DracoSemantic,
    /// 每点几个分量。
    pub components: usize,
    /// `points * components` 个值。归一化整数已经换算到 `[0, 1]` / `[-1, 1]`。
    pub values: Vec<f32>,
}

/// 解码出来的一个网格。
#[derive(Debug, Clone, Default)]
pub struct DracoMesh {
    /// 点数（即顶点数）。
    pub points: usize,
    /// 三角形索引；点云时为空。
    pub indices: Vec<u32>,
    /// 全部属性。
    pub attributes: Vec<DracoAttribute>,
}

impl DracoMesh {
    /// 按唯一 id 找属性。
    pub fn by_id(&self, unique_id: u32) -> Option<&DracoAttribute> {
        self.attributes.iter().find(|a| a.unique_id == unique_id)
    }

    /// 按语义找第一个属性。
    pub fn by_semantic(&self, semantic: DracoSemantic) -> Option<&DracoAttribute> {
        self.attributes.iter().find(|a| a.semantic == semantic)
    }
}

/// 归一化整数的满量程。非整数类型返回 `None`。
fn normalized_range(data_type: DataType) -> Option<f32> {
    Some(match data_type {
        DataType::Int8 => 127.0,
        DataType::Uint8 => 255.0,
        DataType::Int16 => 32767.0,
        DataType::Uint16 => 65535.0,
        DataType::Int32 => 2147483647.0,
        DataType::Uint32 => 4294967295.0,
        _ => return None,
    })
}

/// 解码一段 Draco 字节流。
pub fn decode(bytes: &[u8]) -> Result<DracoMesh, String> {
    let mut buffer = DecoderBuffer::new(bytes);
    let mut decoder = MeshDecoder::new();
    let mut mesh = draco_core::Mesh::new();
    decoder
        .decode(&mut buffer, &mut mesh)
        .map_err(|e| format!("Draco 解码失败：{e:?}"))?;

    let points = mesh.num_points();
    if points > MAX_POINTS {
        return Err(format!("Draco 网格有 {points} 个点，超过上限"));
    }
    let indices = mesh
        .faces()
        .iter()
        .flat_map(|face| face.iter().map(|p| p.0))
        .collect::<Vec<_>>();
    if indices.iter().any(|&i| i as usize >= points) {
        return Err("Draco 网格的面引用了不存在的点".into());
    }

    let mut attributes = Vec::with_capacity(mesh.num_attributes() as usize);
    for id in 0..mesh.num_attributes() {
        let attribute = mesh.attribute(id);
        let components = attribute.num_components() as usize;
        let mut values = attribute.read_f32s(points, components);
        if attribute.normalized()
            && let Some(range) = normalized_range(attribute.data_type())
        {
            let signed = matches!(attribute.data_type(), DataType::Int8 | DataType::Int16 | DataType::Int32);
            for v in &mut values {
                *v = if signed { (*v / range).max(-1.0) } else { *v / range };
            }
        }
        attributes.push(DracoAttribute {
            unique_id: attribute.unique_id(),
            semantic: match attribute.attribute_type() {
                GeometryAttributeType::Position => DracoSemantic::Position,
                GeometryAttributeType::Normal => DracoSemantic::Normal,
                GeometryAttributeType::Color => DracoSemantic::Color,
                GeometryAttributeType::TexCoord => DracoSemantic::TexCoord,
                _ => DracoSemantic::Generic,
            },
            components,
            values,
        });
    }

    Ok(DracoMesh {
        points,
        indices,
        attributes,
    })
}
