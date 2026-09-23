//! glTF 的**导入前处理**：在 gltf-rs 校验之前，把「压缩过的 / 换了壳的」
//! 数据还原成普通 glTF。
//!
//! # 为什么在 JSON 这一层做
//!
//! 这几个扩展有一个共同点：它们只改变数据**怎么存**，不改变数据**是什么**。
//!
//! | 扩展 | 做了什么 | 还原之后 |
//! |---|---|---|
//! | `EXT_meshopt_compression` / `KHR_meshopt_compression` | bufferView 整段压缩 | 一段普通的 bufferView |
//! | `KHR_draco_mesh_compression` | 图元的属性和索引整体压缩 | 几个普通的 float / u32 accessor |
//! | `KHR_mesh_quantization` | 顶点属性用整数存 | float accessor |
//! | `KHR_texture_basisu` / `EXT_texture_avif` / `EXT_texture_webp` | `texture.source` 挪进了扩展 | 普通的 `texture.source` |
//!
//! 所以最省事、也最不容易出错的做法是：**先把 JSON 和缓冲改写成等价的
//! 未压缩 glTF**，再交给后面那条已经测透了的导入路径。后面的网格、材质、
//! 动画代码因此一行都不用知道这些扩展的存在。
//!
//! 反过来，如果在读 accessor 的地方逐个特判，每加一个扩展就要改遍所有
//! 读属性的地方，而漏掉的那一处只会表现成「某个模型的法线是乱的」。
//!
//! 解出来的数据统一追加到一块新缓冲（「合成缓冲」）里，放在缓冲列表末尾。

use crate::{draco, meshopt, uri};
use kasset::{LoadError, ResourceIo};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, path::Path, sync::Arc};

/// 前处理处理掉、因而可以从 `extensionsRequired` 里划掉的扩展。
pub(crate) const HANDLED: &[&str] = &[
    "EXT_meshopt_compression",
    "KHR_meshopt_compression",
    "KHR_draco_mesh_compression",
    "KHR_mesh_quantization",
    "KHR_texture_basisu",
    "EXT_texture_avif",
    "EXT_texture_webp",
    "KHR_animation_pointer",
];

/// 一条 `KHR_animation_pointer` 通道，已经从 JSON 里摘出来并解码。
///
/// gltf-rs 的 `animation.channel.target` 要求有 `node`、`path` 只认 TRS 与
/// `weights`，指针通道（`path: "pointer"`、没有 `node`）会让整个文件反序列化
/// 失败。所以这里在交给它之前把这些通道摘掉，数据自己解码，由导入器
/// 按指针路径翻译成轨道。
#[derive(Debug, Clone)]
pub(crate) struct PointerChannel {
    /// 属于第几个动画。
    pub animation: usize,
    /// JSON 指针，例如 `/materials/0/pbrMetallicRoughness/baseColorFactor`。
    pub pointer: String,
    /// 关键帧时刻。
    pub times: Vec<f32>,
    /// 采样值，逐帧 `components` 个（三次样条时每帧三组）。
    pub values: Vec<f32>,
    /// 每个值几个分量。
    pub components: usize,
    /// `LINEAR` / `STEP` / `CUBICSPLINE`。
    pub interpolation: String,
}

/// 把文件拆成 JSON 和 GLB 的 BIN 块。
pub(crate) fn split(bytes: &[u8]) -> Result<(Value, Option<Vec<u8>>), LoadError> {
    if bytes.starts_with(b"glTF") {
        let glb = gltf::Glb::from_slice(bytes).map_err(LoadError::custom)?;
        let json = serde_json::from_slice(&glb.json).map_err(LoadError::custom)?;
        Ok((json, glb.bin.map(|b| b.into_owned())))
    } else {
        Ok((serde_json::from_slice(bytes).map_err(LoadError::custom)?, None))
    }
}

fn array<'a>(json: &'a Value, key: &str) -> &'a [Value] {
    json.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn usize_of(value: &Value, key: &str) -> Option<usize> {
    value.get(key).and_then(Value::as_u64).map(|v| v as usize)
}

fn extension<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.get("extensions")?.get(name)
}

fn remove_extension(value: &mut Value, name: &str) {
    if let Some(extensions) = value.get_mut("extensions").and_then(Value::as_object_mut) {
        extensions.remove(name);
        if extensions.is_empty() {
            value.as_object_mut().map(|o| o.remove("extensions"));
        }
    }
}

/// 读全部缓冲。
///
/// meshopt 允许声明一块**不存在**的「回退缓冲」（`fallback: true`，通常没有
/// `uri`）：它只是给不认识扩展的加载器一个占位。这里认识扩展，用不到
/// 它的内容，给一块同样长度的零即可。
pub(crate) async fn load_buffers(
    json: &Value,
    mut blob: Option<Vec<u8>>,
    base: &Path,
    io: &Arc<dyn ResourceIo>,
) -> Result<Vec<Vec<u8>>, LoadError> {
    let mut buffers = Vec::new();
    for (index, buffer) in array(json, "buffers").iter().enumerate() {
        let length = usize_of(buffer, "byteLength").unwrap_or(0);
        let fallback = ["EXT_meshopt_compression", "KHR_meshopt_compression"].iter().any(|name| {
            extension(buffer, name)
                .and_then(|e| e.get("fallback"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        let data = if fallback {
            vec![0; length.min(1 << 30)]
        } else if let Some(source) = buffer.get("uri").and_then(Value::as_str) {
            uri::read_uri(source, base, io).await?
        } else if index == 0
            && let Some(bin) = blob.take()
        {
            bin
        } else {
            return Err(LoadError::message("glTF 引用了 BIN 块，但文件里没有"));
        };
        if data.len() < length {
            return Err(LoadError::message(format!(
                "缓冲区长度不足：声明 {length} 字节，实际 {} 字节",
                data.len()
            )));
        }
        buffers.push(data);
    }
    Ok(buffers)
}

/// 合成缓冲：解码出来的数据都往这里追加。
struct Synthetic {
    index: usize,
    data: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    view_base: usize,
    accessor_base: usize,
    /// 要覆盖回原 accessor 槽位的新 accessor：`(原序号, 内容)`。
    adopted: Vec<(usize, Value)>,
}

impl Synthetic {
    /// 追加一段数据，返回新 bufferView 的序号。
    fn view(&mut self, bytes: &[u8], stride: Option<usize>) -> usize {
        // accessor 要求按分量大小对齐；统一按 4 对齐最省事。
        while self.data.len() % 4 != 0 {
            self.data.push(0);
        }
        let mut view = json!({
            "buffer": self.index,
            "byteOffset": self.data.len(),
            "byteLength": bytes.len(),
        });
        if let Some(stride) = stride {
            view["byteStride"] = json!(stride);
        }
        self.data.extend_from_slice(bytes);
        self.views.push(view);
        self.view_base + self.views.len() - 1
    }

    /// 追加一个 float accessor。
    fn floats(&mut self, values: &[f32], components: usize, with_bounds: bool) -> usize {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, None);
        let count = values.len() / components.max(1);
        let mut accessor = json!({
            "bufferView": view,
            "componentType": 5126,
            "count": count,
            "type": type_name(components),
        });
        if with_bounds && count > 0 {
            let mut min = vec![f32::INFINITY; components];
            let mut max = vec![f32::NEG_INFINITY; components];
            for chunk in values.chunks_exact(components) {
                for (k, v) in chunk.iter().enumerate() {
                    min[k] = min[k].min(*v);
                    max[k] = max[k].max(*v);
                }
            }
            accessor["min"] = json!(min);
            accessor["max"] = json!(max);
        }
        self.accessors.push(accessor);
        self.accessor_base + self.accessors.len() - 1
    }

    /// 让新 accessor 顶替原来的那个槽位，返回应当引用的序号。
    ///
    /// Draco 图元的原 accessor 只有 `count` / `type`、没有 `bufferView`——
    /// gltf-rs 的校验会拒绝这种 accessor，哪怕已经没有人引用它。所以不能
    /// 只是把图元改指向新 accessor，得把原槽位也填上真数据。
    fn adopt(&mut self, accessor: usize, original: Option<usize>) -> usize {
        match original {
            Some(slot) if slot < self.accessor_base => {
                let value = self.accessors[accessor - self.accessor_base].clone();
                self.adopted.push((slot, value));
                slot
            }
            _ => accessor,
        }
    }

    /// 追加一个整数 accessor（索引用 u32，关节号用 u16）。
    fn integers(&mut self, values: &[u32], components: usize, component_type: u32) -> usize {
        let bytes: Vec<u8> = match component_type {
            5123 => values.iter().flat_map(|v| (*v as u16).to_le_bytes()).collect(),
            _ => values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        let view = self.view(&bytes, None);
        self.accessors.push(json!({
            "bufferView": view,
            "componentType": component_type,
            "count": values.len() / components.max(1),
            "type": type_name(components),
        }));
        self.accessor_base + self.accessors.len() - 1
    }
}

fn type_name(components: usize) -> &'static str {
    match components {
        1 => "SCALAR",
        2 => "VEC2",
        3 => "VEC3",
        4 => "VEC4",
        9 => "MAT3",
        16 => "MAT4",
        _ => "SCALAR",
    }
}

fn components_of(kind: &str) -> usize {
    match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" | "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => 1,
    }
}

fn component_size(component_type: u64) -> usize {
    match component_type {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        _ => 4,
    }
}

/// 执行全部前处理，返回摘出来的动画指针通道。
pub(crate) fn run(json: &mut Value, buffers: &mut Vec<Vec<u8>>) -> Result<Vec<PointerChannel>, LoadError> {
    redirect_texture_sources(json);

    // meshopt 先解、先落成一块独立的缓冲：量化反解和 Draco 要从解压后的
    // bufferView 里读数据（`coffeemat.glb` 就是 meshopt + 量化叠在一起）。
    let unpacked = decompress_meshopt(json, buffers, buffers.len())?;
    if !unpacked.is_empty() {
        let object = json.as_object_mut().ok_or_else(|| LoadError::message("glTF 顶层不是对象"))?;
        push_all(object, "buffers", vec![json!({ "byteLength": unpacked.len() })]);
        buffers.push(unpacked);
    }

    let mut synthetic = Synthetic {
        index: buffers.len(),
        data: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
        view_base: array(json, "bufferViews").len(),
        accessor_base: array(json, "accessors").len(),
        adopted: Vec::new(),
    };

    decompress_draco(json, buffers, &mut synthetic)?;
    for (slot, accessor) in std::mem::take(&mut synthetic.adopted) {
        json["accessors"][slot] = accessor;
    }
    dequantize(json, buffers, &mut synthetic);

    if !synthetic.data.is_empty() {
        let object = json.as_object_mut().ok_or_else(|| LoadError::message("glTF 顶层不是对象"))?;
        push_all(object, "bufferViews", synthetic.views);
        push_all(object, "accessors", synthetic.accessors);
        push_all(object, "buffers", vec![json!({ "byteLength": synthetic.data.len() })]);
        buffers.push(synthetic.data);
    }
    Ok(extract_pointer_channels(json, buffers))
}

/// 把 `KHR_animation_pointer` 通道（以及任何缺 `node` 的通道）从 JSON 里摘掉并解码。
fn extract_pointer_channels(json: &mut Value, buffers: &[Vec<u8>]) -> Vec<PointerChannel> {
    let mut out = Vec::new();
    let animation_count = array(json, "animations").len();
    for animation in 0..animation_count {
        let samplers = array(&json["animations"][animation], "samplers").to_vec();
        let Some(channels) = json["animations"][animation].get_mut("channels").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut kept = Vec::with_capacity(channels.len());
        let mut taken = Vec::new();
        for channel in channels.drain(..) {
            let target = channel.get("target").cloned().unwrap_or(Value::Null);
            let is_pointer = target.get("path").and_then(Value::as_str) == Some("pointer")
                || target.get("node").is_none();
            if is_pointer {
                taken.push(channel);
            } else {
                kept.push(channel);
            }
        }
        *channels = kept;
        for channel in taken {
            let Some(pointer) = extension(&channel["target"], "KHR_animation_pointer")
                .and_then(|e| e.get("pointer"))
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                klog::warn!("动画 {animation} 有一条既没有 node 也没有指针的通道，已跳过");
                continue;
            };
            let Some(sampler) = usize_of(&channel, "sampler").and_then(|i| samplers.get(i)) else {
                continue;
            };
            let input = usize_of(sampler, "input").and_then(|a| read_accessor(json, buffers, a));
            let output = usize_of(sampler, "output").and_then(|a| read_accessor(json, buffers, a));
            let (Some((times, _)), Some((values, components))) = (input, output) else {
                klog::warn!("动画指针 {pointer} 的采样数据读不出来，已跳过");
                continue;
            };
            out.push(PointerChannel {
                animation,
                pointer,
                times,
                values,
                components,
                interpolation: sampler
                    .get("interpolation")
                    .and_then(Value::as_str)
                    .unwrap_or("LINEAR")
                    .to_string(),
            });
        }
    }
    out
}

fn push_all(object: &mut Map<String, Value>, key: &str, items: Vec<Value>) {
    let entry = object.entry(key).or_insert_with(|| Value::Array(Vec::new()));
    if let Some(list) = entry.as_array_mut() {
        list.extend(items);
    }
}

/// `KHR_texture_basisu` / `EXT_texture_avif` / `EXT_texture_webp`：把扩展里
/// 的 `source` 提到 `texture.source`。
///
/// 扩展里的图片优先于原本的 `source`（那一份通常是给不认识扩展的加载器
/// 准备的 PNG 回退）——既然引擎能解，就用文件作者真正想用的那份。
fn redirect_texture_sources(json: &mut Value) {
    let Some(textures) = json.get_mut("textures").and_then(Value::as_array_mut) else {
        return;
    };
    for texture in textures {
        for name in ["KHR_texture_basisu", "EXT_texture_avif", "EXT_texture_webp"] {
            if let Some(source) = extension(texture, name).and_then(|e| e.get("source")).cloned() {
                texture["source"] = source;
                remove_extension(texture, name);
                break;
            }
        }
    }
}

fn view_bytes<'a>(json: &Value, buffers: &'a [Vec<u8>], view: usize) -> Result<(&'a [u8], Option<usize>), LoadError> {
    let view = array(json, "bufferViews")
        .get(view)
        .ok_or_else(|| LoadError::message("bufferView 序号越界"))?;
    let buffer = usize_of(view, "buffer").unwrap_or(0);
    let offset = usize_of(view, "byteOffset").unwrap_or(0);
    let length = usize_of(view, "byteLength").unwrap_or(0);
    let data = buffers
        .get(buffer)
        .and_then(|b| b.get(offset..offset.checked_add(length)?))
        .ok_or_else(|| LoadError::message("bufferView 越界"))?;
    Ok((data, usize_of(view, "byteStride")))
}

/// 解压所有 meshopt 压缩的 bufferView，返回要作为第 `target` 块缓冲追加的数据。
fn decompress_meshopt(json: &mut Value, buffers: &[Vec<u8>], target: usize) -> Result<Vec<u8>, LoadError> {
    let mut out = Vec::new();
    let count = array(json, "bufferViews").len();
    for index in 0..count {
        let view = &array(json, "bufferViews")[index];
        let Some((name, ext)) = ["EXT_meshopt_compression", "KHR_meshopt_compression"]
            .iter()
            .find_map(|name| extension(view, name).map(|e| (*name, e.clone())))
        else {
            continue;
        };
        let buffer = usize_of(&ext, "buffer").unwrap_or(0);
        let offset = usize_of(&ext, "byteOffset").unwrap_or(0);
        let length = usize_of(&ext, "byteLength").unwrap_or(0);
        let stride = usize_of(&ext, "byteStride").unwrap_or(0);
        let elements = usize_of(&ext, "count").unwrap_or(0);
        let source = buffers
            .get(buffer)
            .and_then(|b| b.get(offset..offset.checked_add(length)?))
            .ok_or_else(|| LoadError::message("meshopt 压缩数据越界"))?;
        let mode = match ext.get("mode").and_then(Value::as_str) {
            Some("ATTRIBUTES") => meshopt::Mode::Attributes,
            Some("TRIANGLES") => meshopt::Mode::Triangles,
            Some("INDICES") => meshopt::Mode::Indices,
            other => return Err(LoadError::message(format!("未知的 meshopt 模式 {other:?}"))),
        };
        let filter = match ext.get("filter").and_then(Value::as_str).unwrap_or("NONE") {
            "NONE" => meshopt::Filter::None,
            "OCTAHEDRAL" => meshopt::Filter::Octahedral,
            "QUATERNION" => meshopt::Filter::Quaternion,
            "EXPONENTIAL" => meshopt::Filter::Exponential,
            "COLOR" => meshopt::Filter::Color,
            other => return Err(LoadError::message(format!("未知的 meshopt 滤波器 {other}"))),
        };
        let decoded = meshopt::decode(source, elements, stride, mode, filter).map_err(LoadError::custom)?;

        // 原地改写这个 bufferView：指向解压缓冲，去掉扩展。
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let at = out.len();
        out.extend_from_slice(&decoded);
        let view = &mut json["bufferViews"][index];
        view["buffer"] = json!(target);
        view["byteOffset"] = json!(at);
        view["byteLength"] = json!(decoded.len());
        remove_extension(view, name);
    }
    Ok(out)
}

fn decompress_draco(json: &mut Value, buffers: &[Vec<u8>], synthetic: &mut Synthetic) -> Result<(), LoadError> {
    let mesh_count = array(json, "meshes").len();
    for mesh in 0..mesh_count {
        let primitive_count = array(&json["meshes"][mesh], "primitives").len();
        for primitive in 0..primitive_count {
            let source = json["meshes"][mesh]["primitives"][primitive].clone();
            let source = &source;
            let Some(ext) = extension(source, "KHR_draco_mesh_compression").cloned() else {
                continue;
            };
            let view = usize_of(&ext, "bufferView").ok_or_else(|| LoadError::message("Draco 扩展缺少 bufferView"))?;
            let (bytes, _) = view_bytes(json, buffers, view)?;
            let decoded = draco::decode(bytes).map_err(LoadError::message)?;

            let mut attributes = source.get("attributes").cloned().unwrap_or_else(|| json!({}));
            let empty = Map::new();
            let mapping = ext.get("attributes").and_then(Value::as_object).unwrap_or(&empty);
            for (semantic, id) in mapping {
                let Some(attribute) = id.as_u64().and_then(|id| decoded.by_id(id as u32)) else {
                    klog::warn!("Draco 扩展声明了 {semantic}，但解码结果里没有这个属性");
                    continue;
                };
                let accessor = if semantic.starts_with("JOINTS_") {
                    let values: Vec<u32> = attribute.values.iter().map(|v| *v as u32).collect();
                    synthetic.integers(&values, attribute.components, 5123)
                } else {
                    synthetic.floats(&attribute.values, attribute.components, semantic == "POSITION")
                };
                let original = attributes.get(semantic).and_then(Value::as_u64).map(|v| v as usize);
                attributes[semantic] = json!(synthetic.adopt(accessor, original));
            }
            let original_indices = usize_of(source, "indices");
            let indices = (!decoded.indices.is_empty())
                .then(|| synthetic.integers(&decoded.indices, 1, 5125))
                .map(|accessor| synthetic.adopt(accessor, original_indices));
            let target = &mut json["meshes"][mesh]["primitives"][primitive];
            target["attributes"] = attributes;
            if let Some(indices) = indices {
                target["indices"] = json!(indices);
            }
            remove_extension(target, "KHR_draco_mesh_compression");
        }
    }
    Ok(())
}

/// 读一个 accessor 成 `f32`，按 `normalized` 换算。稀疏 accessor 返回 `None`。
fn read_accessor(json: &Value, buffers: &[Vec<u8>], index: usize) -> Option<(Vec<f32>, usize)> {
    let accessor = array(json, "accessors").get(index)?;
    if accessor.get("sparse").is_some() {
        return None;
    }
    let component_type = accessor.get("componentType")?.as_u64()?;
    let components = components_of(accessor.get("type")?.as_str()?);
    let count = usize_of(accessor, "count")?;
    let normalized = accessor.get("normalized").and_then(Value::as_bool).unwrap_or(false);
    let (data, stride) = view_bytes(json, buffers, usize_of(accessor, "bufferView")?).ok()?;
    let offset = usize_of(accessor, "byteOffset").unwrap_or(0);
    let size = component_size(component_type);
    let stride = stride.unwrap_or(size * components);
    let mut values = Vec::with_capacity(count * components);
    for element in 0..count {
        for k in 0..components {
            let at = offset + element * stride + k * size;
            let raw = data.get(at..at + size)?;
            let value = match component_type {
                5120 => {
                    let v = raw[0] as i8 as f32;
                    if normalized { (v / 127.0).max(-1.0) } else { v }
                }
                5121 => {
                    let v = raw[0] as f32;
                    if normalized { v / 255.0 } else { v }
                }
                5122 => {
                    let v = i16::from_le_bytes([raw[0], raw[1]]) as f32;
                    if normalized { (v / 32767.0).max(-1.0) } else { v }
                }
                5123 => {
                    let v = u16::from_le_bytes([raw[0], raw[1]]) as f32;
                    if normalized { v / 65535.0 } else { v }
                }
                5125 => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f32,
                _ => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            };
            values.push(value);
        }
    }
    Some((values, components))
}

/// `KHR_mesh_quantization`：把整数存的位置 / 法线 / 切线 / UV 转成 float。
///
/// gltf-rs 的读取器对这几种属性**只认 float**（UV 另外认归一化的 u8 / u16），
/// 遇到 i8 / i16 会按 float 的字节宽度去读，读出来的是垃圾；UV 遇到有符号
/// 类型干脆 `unreachable!()`。所以这里统一转掉，颜色、关节、权重那几种
/// 读取器本来就认整数的不动。
fn dequantize(json: &mut Value, buffers: &[Vec<u8>], synthetic: &mut Synthetic) {
    let is_float = |json: &Value, accessor: usize| {
        array(json, "accessors")
            .get(accessor)
            .and_then(|a| a.get("componentType"))
            .and_then(Value::as_u64)
            == Some(5126)
    };
    let wanted = |semantic: &str| {
        matches!(semantic, "POSITION" | "NORMAL" | "TANGENT") || semantic.starts_with("TEXCOORD_")
    };
    let mut converted: HashMap<usize, usize> = HashMap::new();

    let mesh_count = array(json, "meshes").len();
    for mesh in 0..mesh_count {
        let primitive_count = array(&json["meshes"][mesh], "primitives").len();
        for primitive in 0..primitive_count {
            // 属性表本身 + 每个形变目标的属性表。
            let mut tables = vec![vec!["attributes".to_string()]];
            let target_count = array(&json["meshes"][mesh]["primitives"][primitive], "targets").len();
            for target in 0..target_count {
                tables.push(vec!["targets".into(), target.to_string()]);
            }
            for path in tables {
                let table = {
                    let mut node = &json["meshes"][mesh]["primitives"][primitive];
                    for key in &path {
                        node = match key.parse::<usize>() {
                            Ok(i) => &node[i],
                            Err(_) => &node[key.as_str()],
                        };
                    }
                    node.as_object().cloned().unwrap_or_default()
                };
                for (semantic, value) in table {
                    let Some(accessor) = value.as_u64().map(|v| v as usize) else { continue };
                    if !wanted(&semantic) || is_float(json, accessor) {
                        continue;
                    }
                    let replacement = match converted.get(&accessor) {
                        Some(&done) => done,
                        None => {
                            let Some((values, components)) = read_accessor(json, buffers, accessor) else {
                                klog::warn!("量化属性 {semantic} 读不出来（稀疏或越界），保持原样");
                                continue;
                            };
                            // 切线的 w 是手性符号，归一化后是 ±1，不需要特殊处理。
                            let done = synthetic.floats(&values, components, semantic == "POSITION" && path.len() == 1);
                            converted.insert(accessor, done);
                            done
                        }
                    };
                    let mut node = &mut json["meshes"][mesh]["primitives"][primitive];
                    for key in &path {
                        node = match key.parse::<usize>() {
                            Ok(i) => &mut node[i],
                            Err(_) => &mut node[key.as_str()],
                        };
                    }
                    node[semantic.as_str()] = json!(replacement);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantized_positions_become_floats() {
        // 一个三角形，位置用 i16 存（非归一化）。
        let raw: Vec<u8> = [1i16, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut json = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": raw.len()}],
            "bufferViews": [{"buffer": 0, "byteLength": raw.len(), "byteStride": 8}],
            "accessors": [{"bufferView": 0, "componentType": 5122, "count": 3, "type": "VEC3"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        });
        let mut buffers = vec![raw];
        run(&mut json, &mut buffers).unwrap();
        let accessor = json["meshes"][0]["primitives"][0]["attributes"]["POSITION"].as_u64().unwrap() as usize;
        let (values, components) = read_accessor(&json, &buffers, accessor).unwrap();
        assert_eq!(components, 3);
        assert_eq!(values, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(json["accessors"][accessor]["max"], json!([7.0, 8.0, 9.0]));
    }

    #[test]
    fn basisu_source_is_lifted_out_of_the_extension() {
        let mut json = json!({
            "textures": [{"extensions": {"KHR_texture_basisu": {"source": 3}}}],
        });
        redirect_texture_sources(&mut json);
        assert_eq!(json["textures"][0]["source"], json!(3));
        assert!(json["textures"][0].get("extensions").is_none());
    }
}
