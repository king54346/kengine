//! `EXT_meshopt_compression` / `KHR_meshopt_compression` 的解码器。
//!
//! meshoptimizer 的压缩是**按 bufferView** 做的：一段顶点属性或索引被整体
//! 编码成一串字节，解出来就是原本那段 bufferView 的内容，逐字节一致。
//! 所以它不牵涉网格语义，这里也只负责「字节进、字节出」，
//! 写回 glTF 的事由 [`crate::prepare`] 来做。
//!
//! 对照的是 meshoptimizer 仓库里的 `vertexcodec.cpp` / `indexcodec.cpp` /
//! `vertexfilter.cpp` 的标量路径（没有 SIMD 分支，逻辑完全相同）。
//!
//! | 模式 | 编码 |
//! |---|---|
//! | `ATTRIBUTES` | 顶点编解码器 v0 / v1：按字节通道做增量 + 分组变长位宽 |
//! | `TRIANGLES` | 索引编解码器 v0 / v1：边 FIFO + 顶点 FIFO |
//! | `INDICES` | 索引序列：两条基线的 zigzag 增量 |
//!
//! 滤波器 `OCTAHEDRAL` / `QUATERNION` / `EXPONENTIAL` / `COLOR` 都支持。
//!
//! # 面对坏数据
//!
//! 解码的输入是外部文件。所有读取都做了边界检查，越界一律返回错误，
//! 不会 panic，也不会读出界外的字节——C++ 版靠「尾部留 16 字节余量」省掉
//! 了逐字节检查，这里换成了切片的边界检查。

/// 解码失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshoptError(pub &'static str);

impl std::fmt::Display for MeshoptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "meshopt 解码失败：{}", self.0)
    }
}

impl std::error::Error for MeshoptError {}

type Result<T> = std::result::Result<T, MeshoptError>;

/// bufferView 的压缩模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 顶点属性。
    Attributes,
    /// 三角形索引。
    Triangles,
    /// 任意索引序列。
    Indices,
}

/// 解码后要套的滤波器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// 不滤波。
    None,
    /// 八面体编码的单位向量（法线、切线）。
    Octahedral,
    /// 丢掉最大分量的单位四元数。
    Quaternion,
    /// 共享指数的浮点数。
    Exponential,
    /// YCoCg 编码的颜色。
    Color,
}

/// 按扩展里的 `mode` / `filter` 解码一段 bufferView。
///
/// `count` 是元素个数，`stride` 是每个元素的字节数，
/// 返回值长度恰好是 `count * stride`。
pub fn decode(source: &[u8], count: usize, stride: usize, mode: Mode, filter: Filter) -> Result<Vec<u8>> {
    let size = count.checked_mul(stride).ok_or(MeshoptError("尺寸溢出"))?;
    if size > 1 << 30 {
        return Err(MeshoptError("解码后超过 1 GiB"));
    }
    let mut out = vec![0u8; size];
    match mode {
        Mode::Attributes => decode_vertex_buffer(&mut out, count, stride, source)?,
        Mode::Triangles => decode_index_buffer(&mut out, count, stride, source)?,
        Mode::Indices => decode_index_sequence(&mut out, count, stride, source)?,
    }
    match filter {
        Filter::None => {}
        Filter::Octahedral => filter_oct(&mut out, count, stride)?,
        Filter::Quaternion => filter_quat(&mut out, count, stride)?,
        Filter::Exponential => filter_exp(&mut out, stride)?,
        Filter::Color => filter_color(&mut out, count, stride)?,
    }
    Ok(out)
}

// ── 顶点编解码器 ──

const VERTEX_HEADER: u8 = 0xa0;
const VERTEX_BLOCK_SIZE_BYTES: usize = 8192;
const VERTEX_BLOCK_MAX_SIZE: usize = 256;
const BYTE_GROUP_SIZE: usize = 16;
const BYTE_GROUP_DECODE_LIMIT: usize = 24;
const TAIL_MIN_SIZE_V0: usize = 32;
const TAIL_MIN_SIZE_V1: usize = 24;
const BITS_V0: [u32; 4] = [0, 2, 4, 8];
const BITS_V1: [u32; 5] = [0, 1, 2, 4, 8];

fn vertex_block_size(vertex_size: usize) -> usize {
    ((VERTEX_BLOCK_SIZE_BYTES / vertex_size) & !(BYTE_GROUP_SIZE - 1)).min(VERTEX_BLOCK_MAX_SIZE)
}

/// 一个带位置的只读游标。所有读取都检查边界。
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    /// 允许读到的上限（不含）。顶点解码时它停在尾部之前。
    end: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.end.saturating_sub(self.at)
    }

    fn byte(&mut self) -> Result<u8> {
        if self.at >= self.end {
            return Err(MeshoptError("数据被截断"));
        }
        let value = self.data[self.at];
        self.at += 1;
        Ok(value)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(MeshoptError("数据被截断"));
        }
        let slice = &self.data[self.at..self.at + n];
        self.at += n;
        Ok(slice)
    }
}

fn decode_bytes_group(reader: &mut Reader<'_>, out: &mut [u8], bits: u32) -> Result<()> {
    match bits {
        0 => out[..BYTE_GROUP_SIZE].fill(0),
        8 => out[..BYTE_GROUP_SIZE].copy_from_slice(reader.take(BYTE_GROUP_SIZE)?),
        1 | 2 | 4 => {
            let header_bytes = (BYTE_GROUP_SIZE * bits as usize) / 8;
            let header = reader.take(header_bytes)?;
            let per_byte = 8 / bits as usize;
            let escape = (1u8 << bits) - 1;
            for i in 0..BYTE_GROUP_SIZE {
                let mut byte = header[i / per_byte];
                // 1 位的分组在字节内是**反着**排的（低位在前），其余是高位在前。
                if bits == 1 {
                    byte = byte.reverse_bits();
                }
                let shift = 8 - bits as usize * (i % per_byte + 1);
                let enc = (byte >> shift) & escape;
                out[i] = if enc == escape { reader.byte()? } else { enc };
            }
        }
        _ => return Err(MeshoptError("非法的分组位宽")),
    }
    Ok(())
}

fn decode_bytes(reader: &mut Reader<'_>, out: &mut [u8], bits: &[u32]) -> Result<()> {
    let size = out.len();
    let header_size = (size / BYTE_GROUP_SIZE).div_ceil(4);
    let header = reader.take(header_size)?;
    for (group, chunk) in out.chunks_exact_mut(BYTE_GROUP_SIZE).enumerate() {
        if reader.remaining() < BYTE_GROUP_DECODE_LIMIT {
            return Err(MeshoptError("数据被截断"));
        }
        let selector = (header[group / 4] >> ((group % 4) * 2)) & 3;
        let width = *bits.get(selector as usize).ok_or(MeshoptError("非法的位宽选择"))?;
        decode_bytes_group(reader, chunk, width)?;
    }
    Ok(())
}

fn unzigzag8(v: u8) -> u8 {
    (0u8.wrapping_sub(v & 1)) ^ (v >> 1)
}

fn unzigzag16(v: u16) -> u16 {
    (0u16.wrapping_sub(v & 1)) ^ (v >> 1)
}

#[allow(clippy::too_many_arguments)]
fn decode_vertex_block(
    reader: &mut Reader<'_>,
    vertex_data: &mut [u8],
    vertex_count: usize,
    vertex_size: usize,
    last_vertex: &mut [u8; 256],
    channels: Option<&[u8]>,
    version: u8,
) -> Result<()> {
    let aligned = vertex_count.div_ceil(BYTE_GROUP_SIZE) * BYTE_GROUP_SIZE;
    let mut buffer = [0u8; VERTEX_BLOCK_MAX_SIZE * 4];
    let control_size = if version == 0 { 0 } else { vertex_size / 4 };
    let control = reader.take(control_size)?;

    for k in (0..vertex_size).step_by(4) {
        let ctrl_byte = if version == 0 { 0 } else { control[k / 4] };
        for j in 0..4 {
            let ctrl = (ctrl_byte >> (j * 2)) & 3;
            let lane = &mut buffer[j * vertex_count..j * vertex_count + aligned.max(vertex_count)];
            match ctrl {
                3 => lane[..vertex_count].copy_from_slice(reader.take(vertex_count)?),
                2 => lane[..vertex_count].fill(0),
                _ => {
                    // 解码按 16 对齐写，可能越过本通道写进下一通道的开头；
                    // 下一通道随后会被自己的数据覆盖，和 C++ 的行为一致。
                    let bits: &[u32] = if version == 0 { &BITS_V0 } else { &BITS_V1[ctrl as usize..] };
                    let mut scratch = [0u8; VERTEX_BLOCK_MAX_SIZE];
                    decode_bytes(reader, &mut scratch[..aligned], bits)?;
                    lane[..vertex_count].copy_from_slice(&scratch[..vertex_count]);
                }
            }
        }

        let channel = channels.map_or(0, |c| c[k / 4]);
        let last = &mut last_vertex[k..k + 4];
        match channel & 3 {
            0 => {
                for byte in 0..4 {
                    let mut p = last[byte];
                    for i in 0..vertex_count {
                        let v = unzigzag8(buffer[byte * vertex_count + i]).wrapping_add(p);
                        vertex_data[i * vertex_size + k + byte] = v;
                        p = v;
                    }
                }
            }
            1 => {
                for half in 0..2 {
                    let mut p = u16::from_le_bytes([last[half * 2], last[half * 2 + 1]]);
                    let base = half * 2 * vertex_count;
                    for i in 0..vertex_count {
                        let raw = u16::from_le_bytes([buffer[base + i], buffer[base + vertex_count + i]]);
                        let v = unzigzag16(raw).wrapping_add(p);
                        vertex_data[i * vertex_size + k + half * 2..][..2].copy_from_slice(&v.to_le_bytes());
                        p = v;
                    }
                }
            }
            2 => {
                let rot = (32 - (channel >> 4) as u32) & 31;
                let mut p = u32::from_le_bytes([last[0], last[1], last[2], last[3]]);
                for i in 0..vertex_count {
                    let raw = u32::from_le_bytes([
                        buffer[i],
                        buffer[vertex_count + i],
                        buffer[vertex_count * 2 + i],
                        buffer[vertex_count * 3 + i],
                    ]);
                    let v = raw.rotate_left(rot) ^ p;
                    vertex_data[i * vertex_size + k..][..4].copy_from_slice(&v.to_le_bytes());
                    p = v;
                }
            }
            _ => return Err(MeshoptError("非法的通道类型")),
        }
    }

    last_vertex[..vertex_size]
        .copy_from_slice(&vertex_data[vertex_size * (vertex_count - 1)..vertex_size * vertex_count]);
    Ok(())
}

/// `meshopt_decodeVertexBuffer`。
pub fn decode_vertex_buffer(out: &mut [u8], vertex_count: usize, vertex_size: usize, buffer: &[u8]) -> Result<()> {
    if vertex_size == 0 || vertex_size > 256 || vertex_size % 4 != 0 {
        return Err(MeshoptError("顶点步长必须是 4 的倍数且不超过 256"));
    }
    let header = *buffer.first().ok_or(MeshoptError("数据为空"))?;
    if header & 0xf0 != VERTEX_HEADER {
        return Err(MeshoptError("不是顶点编码的数据"));
    }
    let version = header & 0x0f;
    if version > 1 {
        return Err(MeshoptError("不支持的顶点编码版本"));
    }
    let tail_size = vertex_size + if version == 0 { 0 } else { vertex_size / 4 };
    let tail_min = if version == 0 { TAIL_MIN_SIZE_V0 } else { TAIL_MIN_SIZE_V1 };
    let tail_pad = tail_size.max(tail_min);
    if buffer.len() - 1 < tail_pad {
        return Err(MeshoptError("数据被截断"));
    }
    let tail = &buffer[buffer.len() - tail_size..];
    let mut last_vertex = [0u8; 256];
    last_vertex[..vertex_size].copy_from_slice(&tail[..vertex_size]);
    let channels = (version != 0).then(|| &tail[vertex_size..]);

    let block = vertex_block_size(vertex_size);
    let mut reader = Reader { data: buffer, at: 1, end: buffer.len() };
    let mut offset = 0;
    while offset < vertex_count {
        let size = block.min(vertex_count - offset);
        decode_vertex_block(
            &mut reader,
            &mut out[offset * vertex_size..(offset + size) * vertex_size],
            size,
            vertex_size,
            &mut last_vertex,
            channels,
            version,
        )?;
        offset += size;
    }
    if reader.remaining() != tail_pad {
        return Err(MeshoptError("数据长度与顶点数不符"));
    }
    Ok(())
}

// ── 索引编解码器 ──

const INDEX_HEADER: u8 = 0xe0;
const SEQUENCE_HEADER: u8 = 0xd0;

fn decode_vbyte(reader: &mut Reader<'_>) -> Result<u32> {
    let lead = reader.byte()?;
    if lead < 128 {
        return Ok(lead as u32);
    }
    let mut result = (lead & 127) as u32;
    let mut shift = 7;
    for _ in 0..4 {
        let group = reader.byte()?;
        result |= ((group & 127) as u32) << shift;
        shift += 7;
        if group < 128 {
            break;
        }
    }
    Ok(result)
}

fn decode_index(reader: &mut Reader<'_>, last: u32) -> Result<u32> {
    let v = decode_vbyte(reader)?;
    let d = (v >> 1) ^ 0u32.wrapping_sub(v & 1);
    Ok(last.wrapping_add(d))
}

fn write_index(out: &mut [u8], index_size: usize, at: usize, value: u32) {
    if index_size == 2 {
        out[at * 2..at * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes());
    } else {
        out[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// `meshopt_decodeIndexBuffer`。
pub fn decode_index_buffer(out: &mut [u8], index_count: usize, index_size: usize, buffer: &[u8]) -> Result<()> {
    if index_count % 3 != 0 || !(index_size == 2 || index_size == 4) {
        return Err(MeshoptError("三角形索引数必须是 3 的倍数，宽度 2 或 4 字节"));
    }
    if buffer.len() < 1 + index_count / 3 + 16 {
        return Err(MeshoptError("数据被截断"));
    }
    if buffer[0] & 0xf0 != INDEX_HEADER {
        return Err(MeshoptError("不是索引编码的数据"));
    }
    let version = buffer[0] & 0x0f;
    if version > 1 {
        return Err(MeshoptError("不支持的索引编码版本"));
    }

    let mut edges = [[u32::MAX; 2]; 16];
    let mut vertices = [u32::MAX; 16];
    let (mut edge_at, mut vertex_at) = (0usize, 0usize);
    let (mut next, mut last) = (0u32, 0u32);
    let fecmax = if version >= 1 { 13 } else { 15 };

    let codes = &buffer[1..1 + index_count / 3];
    let safe_end = buffer.len() - 16;
    let aux_table = &buffer[safe_end..];
    let mut data = Reader { data: buffer, at: 1 + index_count / 3, end: safe_end };

    let push_vertex = |fifo: &mut [u32; 16], at: &mut usize, v: u32, cond: bool| {
        fifo[*at] = v;
        *at = (*at + cond as usize) & 15;
    };
    let push_edge = |fifo: &mut [[u32; 2]; 16], at: &mut usize, a: u32, b: u32| {
        fifo[*at] = [a, b];
        *at = (*at + 1) & 15;
    };

    for (triangle, &code) in codes.iter().enumerate() {
        let (a, b, c);
        if code < 0xf0 {
            let fe = (code >> 4) as usize;
            let edge = edges[(edge_at.wrapping_sub(1 + fe)) & 15];
            a = edge[0];
            b = edge[1];
            let fec = (code & 15) as usize;
            if fec < fecmax {
                let cf = vertices[(vertex_at.wrapping_sub(1 + fec)) & 15];
                c = if fec == 0 { next } else { cf };
                next += (fec == 0) as u32;
                push_vertex(&mut vertices, &mut vertex_at, c, fec == 0);
            } else {
                // v1 里 13 / 14 表示「上一个自由索引 ±1」。
                c = if fec != 15 {
                    last.wrapping_add((fec as i32 * 2 - 27) as u32)
                } else {
                    decode_index(&mut data, last)?
                };
                last = c;
                push_vertex(&mut vertices, &mut vertex_at, c, true);
            }
            push_edge(&mut edges, &mut edge_at, c, b);
            push_edge(&mut edges, &mut edge_at, a, c);
        } else if code < 0xfe {
            let aux = aux_table[(code & 15) as usize];
            let feb = (aux >> 4) as usize;
            let fec = (aux & 15) as usize;
            a = next;
            next += 1;
            let bf = vertices[(vertex_at.wrapping_sub(feb)) & 15];
            b = if feb == 0 { next } else { bf };
            next += (feb == 0) as u32;
            let cf = vertices[(vertex_at.wrapping_sub(fec)) & 15];
            c = if fec == 0 { next } else { cf };
            next += (fec == 0) as u32;
            push_vertex(&mut vertices, &mut vertex_at, a, true);
            push_vertex(&mut vertices, &mut vertex_at, b, feb == 0);
            push_vertex(&mut vertices, &mut vertex_at, c, fec == 0);
            push_edge(&mut edges, &mut edge_at, b, a);
            push_edge(&mut edges, &mut edge_at, c, b);
            push_edge(&mut edges, &mut edge_at, a, c);
        } else {
            let aux = data.byte()?;
            let fea = if code == 0xfe { 0 } else { 15 };
            let feb = (aux >> 4) as usize;
            let fec = (aux & 15) as usize;
            if aux == 0 {
                next = 0;
            }
            let mut va = if fea == 0 {
                next += 1;
                next - 1
            } else {
                0
            };
            let mut vb = if feb == 0 {
                next += 1;
                next - 1
            } else {
                vertices[(vertex_at.wrapping_sub(feb)) & 15]
            };
            let mut vc = if fec == 0 {
                next += 1;
                next - 1
            } else {
                vertices[(vertex_at.wrapping_sub(fec)) & 15]
            };
            if fea == 15 {
                va = decode_index(&mut data, last)?;
                last = va;
            }
            if feb == 15 {
                vb = decode_index(&mut data, last)?;
                last = vb;
            }
            if fec == 15 {
                vc = decode_index(&mut data, last)?;
                last = vc;
            }
            (a, b, c) = (va, vb, vc);
            push_vertex(&mut vertices, &mut vertex_at, a, true);
            push_vertex(&mut vertices, &mut vertex_at, b, feb == 0 || feb == 15);
            push_vertex(&mut vertices, &mut vertex_at, c, fec == 0 || fec == 15);
            push_edge(&mut edges, &mut edge_at, b, a);
            push_edge(&mut edges, &mut edge_at, c, b);
            push_edge(&mut edges, &mut edge_at, a, c);
        }
        write_index(out, index_size, triangle * 3, a);
        write_index(out, index_size, triangle * 3 + 1, b);
        write_index(out, index_size, triangle * 3 + 2, c);
    }

    if data.at != safe_end {
        return Err(MeshoptError("数据长度与三角形数不符"));
    }
    Ok(())
}

/// `meshopt_decodeIndexSequence`。
pub fn decode_index_sequence(out: &mut [u8], index_count: usize, index_size: usize, buffer: &[u8]) -> Result<()> {
    if !(index_size == 2 || index_size == 4) {
        return Err(MeshoptError("索引宽度必须是 2 或 4 字节"));
    }
    if buffer.len() < 1 + index_count + 4 {
        return Err(MeshoptError("数据被截断"));
    }
    if buffer[0] & 0xf0 != SEQUENCE_HEADER {
        return Err(MeshoptError("不是索引序列编码的数据"));
    }
    if buffer[0] & 0x0f > 1 {
        return Err(MeshoptError("不支持的索引序列版本"));
    }
    let safe_end = buffer.len() - 4;
    let mut data = Reader { data: buffer, at: 1, end: safe_end };
    let mut last = [0u32; 2];
    for i in 0..index_count {
        let mut v = decode_vbyte(&mut data)?;
        let current = (v & 1) as usize;
        v >>= 1;
        let d = (v >> 1) ^ 0u32.wrapping_sub(v & 1);
        let index = last[current].wrapping_add(d);
        last[current] = index;
        write_index(out, index_size, i, index);
    }
    if data.at != safe_end {
        return Err(MeshoptError("数据长度与索引数不符"));
    }
    Ok(())
}

// ── 滤波器 ──

fn round(v: f32) -> i32 {
    (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i32
}

fn filter_oct(data: &mut [u8], count: usize, stride: usize) -> Result<()> {
    match stride {
        4 => {
            let max = 127.0;
            for i in 0..count {
                let e = &mut data[i * 4..i * 4 + 4];
                let (x, y, z) = oct(e[0] as i8 as f32, e[1] as i8 as f32, e[2] as i8 as f32, max);
                e[0] = x as i8 as u8;
                e[1] = y as i8 as u8;
                e[2] = z as i8 as u8;
            }
        }
        8 => {
            let max = 32767.0;
            for i in 0..count {
                let e = &mut data[i * 8..i * 8 + 8];
                let get = |k: usize| i16::from_le_bytes([e[k * 2], e[k * 2 + 1]]) as f32;
                let (x, y, z) = oct(get(0), get(1), get(2), max);
                for (k, v) in [x, y, z].into_iter().enumerate() {
                    e[k * 2..k * 2 + 2].copy_from_slice(&(v as i16).to_le_bytes());
                }
            }
        }
        _ => return Err(MeshoptError("八面体滤波的步长必须是 4 或 8")),
    }
    Ok(())
}

fn oct(mut x: f32, mut y: f32, z: f32, max: f32) -> (i32, i32, i32) {
    let z = z - x.abs() - y.abs();
    let t = if z >= 0.0 { 0.0 } else { z };
    x += if x >= 0.0 { t } else { -t };
    y += if y >= 0.0 { t } else { -t };
    let l = (x * x + y * y + z * z).sqrt();
    let s = max / l;
    (round(x * s), round(y * s), round(z * s))
}

fn filter_quat(data: &mut [u8], count: usize, stride: usize) -> Result<()> {
    if stride != 8 {
        return Err(MeshoptError("四元数滤波的步长必须是 8"));
    }
    let scale = 32767.0 / 2f32.sqrt();
    for i in 0..count {
        let e = &mut data[i * 8..i * 8 + 8];
        let get = |k: usize| i16::from_le_bytes([e[k * 2], e[k * 2 + 1]]);
        let w_raw = get(3);
        let s = (w_raw | 3) as f32;
        let (x, y, z) = (get(0) as f32, get(1) as f32, get(2) as f32);
        let ww = s * s * 2.0 - x * x - y * y - z * z;
        let w = ww.max(0.0).sqrt();
        let ss = scale / s;
        let xf = round(x * ss) as i16;
        let yf = round(y * ss) as i16;
        let zf = round(z * ss) as i16;
        let wf = (w * ss + 0.5) as i32 as i16;
        let qc = (w_raw & 3) as usize;
        let mut put = |k: usize, v: i16| e[k * 2..k * 2 + 2].copy_from_slice(&v.to_le_bytes());
        put((qc + 1) & 3, xf);
        put((qc + 2) & 3, yf);
        put((qc + 3) & 3, zf);
        put(qc & 3, wf);
    }
    Ok(())
}

fn filter_exp(data: &mut [u8], stride: usize) -> Result<()> {
    if stride % 4 != 0 {
        return Err(MeshoptError("指数滤波的步长必须是 4 的倍数"));
    }
    for word in data.chunks_exact_mut(4) {
        let v = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        let m = ((v << 8) as i32) >> 8;
        let e = (v as i32) >> 24;
        let scale = f32::from_bits(((e + 127) as u32) << 23);
        word.copy_from_slice(&(scale * m as f32).to_bits().to_le_bytes());
    }
    Ok(())
}

fn filter_color(data: &mut [u8], count: usize, stride: usize) -> Result<()> {
    let unpack = |y: i32, co: i32, cg: i32, a_raw: i32, max: f32| {
        let mut scale = a_raw;
        scale |= scale >> 1;
        scale |= scale >> 2;
        scale |= scale >> 4;
        scale |= scale >> 8;
        let r = y + co - cg;
        let g = y + cg;
        let b = y - co - cg;
        let a = ((a_raw << 1) & scale) | (a_raw & 1);
        let ss = max / scale.max(1) as f32;
        [r, g, b, a].map(|v| (v as f32 * ss + 0.5) as i32)
    };
    match stride {
        4 => {
            for i in 0..count {
                let e = &mut data[i * 4..i * 4 + 4];
                let out = unpack(e[0] as i32, e[1] as i8 as i32, e[2] as i8 as i32, e[3] as i32, 255.0);
                for k in 0..4 {
                    e[k] = out[k] as u8;
                }
            }
        }
        8 => {
            for i in 0..count {
                let e = &mut data[i * 8..i * 8 + 8];
                let get = |k: usize| u16::from_le_bytes([e[k * 2], e[k * 2 + 1]]);
                let out = unpack(get(0) as i32, get(1) as i16 as i32, get(2) as i16 as i32, get(3) as i32, 65535.0);
                for k in 0..4 {
                    e[k * 2..k * 2 + 2].copy_from_slice(&(out[k] as u16).to_le_bytes());
                }
            }
        }
        _ => return Err(MeshoptError("颜色滤波的步长必须是 4 或 8")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_sequence_round_trips_a_known_encoding() {
        // meshopt_encodeIndexSequence([0, 1, 2, 5]) 的输出（手工推算）：
        // 每个索引编码成 ((zigzag(delta) << 1) | baseline)，全部走基线 0。
        let mut encoded = vec![0xd1u8];
        let mut last = 0i32;
        for index in [0i32, 1, 2, 5] {
            let d = index - last;
            let zz = ((d << 1) ^ (d >> 31)) as u32;
            encoded.push((zz << 1) as u8);
            last = index;
        }
        encoded.extend_from_slice(&[0; 4]);
        let mut out = vec![0u8; 16];
        decode_index_sequence(&mut out, 4, 4, &encoded).unwrap();
        let values: Vec<u32> = out.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(values, [0, 1, 2, 5]);
    }

    #[test]
    fn garbage_is_rejected_without_panicking() {
        let mut out = vec![0u8; 64];
        assert!(decode_vertex_buffer(&mut out, 4, 16, &[0xa0, 1, 2]).is_err());
        assert!(decode_index_buffer(&mut out, 6, 4, &[0xe0; 10]).is_err());
        assert!(decode_index_sequence(&mut out, 4, 4, &[0x00; 12]).is_err());
    }

    #[test]
    fn exponential_filter_rebuilds_floats() {
        // 尾数 3、指数 -1 → 1.5。
        let v: u32 = (((-1i32) as u32) << 24) | 3;
        let mut data = v.to_le_bytes().to_vec();
        filter_exp(&mut data, 4).unwrap();
        assert_eq!(f32::from_le_bytes(data.try_into().unwrap()), 1.5);
    }
}
