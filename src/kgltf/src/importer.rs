//! glTF 导入实现。

use crate::{
    model::{GltfExtras, MeshPart, Model, ModelNode, ModelSkin, NodeTransform},
    uri,
};
use kanim::{AnimationClip, Channel, Curve, Interpolation, Track};
use kasset::{LoadError, Resource, ResourceIo};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3, Vec4};
use kmesh::{Mesh, MorphDelta, MorphTarget, SkinVertex, Vertex};
use ktexture::Texture;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// 导入一份 glTF / GLB。
pub(crate) async fn import(
    bytes: Vec<u8>,
    path: PathBuf,
    io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    // Validate the standard document after accounting for extensions implemented
    // by this importer, which gltf-rs does not include in its feature registry.
    let parsed = gltf::Gltf::from_slice_without_validation(&bytes).map_err(LoadError::custom)?;
    let mut json = parsed.document.into_json();
    // gltf-rs 的特性注册表里没有这些扩展，但这个导入器认得它们；
    // 不从 `extensionsRequired` 里划掉的话，校验会直接拒绝整个文件。
    json.extensions_required.retain(|ext| {
        !matches!(
            ext.as_str(),
            "EXT_mesh_gpu_instancing"
                | "KHR_materials_dispersion"
                | "KHR_materials_iridescence"
                | "KHR_materials_sheen"
                | "KHR_materials_variants"
                | "KHR_texture_transform"
        )
    });
    let gltf = gltf::Gltf { document: gltf::Document::from_json(json).map_err(LoadError::custom)?, blob: parsed.blob };
    let base = uri::base_dir(&path);

    let buffers = load_buffers(&gltf, &base, &io).await?;
    let textures = load_textures(&gltf, &base, &io, &buffers, &path).await;
    let materials = import_materials(&gltf, &textures);
    let (meshes, primitive_ranges) = import_meshes(&gltf, &buffers, &path)?;
    let (mut nodes, roots) = import_nodes(&gltf, &primitive_ranges);
    import_instances(&gltf, &buffers, &mut nodes)?;
    let variants = import_variants(&gltf, &nodes);
    let skins = import_skins(&gltf, &buffers);
    let animations = import_animations(&gltf, &buffers);

    klog::debug!(
        "glTF 已导入：{}（{} 网格 / {} 材质 / {} 节点 / {} 骨架 / {} 动画）",
        path.display(),
        meshes.len(),
        materials.len(),
        nodes.len(),
        skins.len(),
        animations.len()
    );

    Ok(Model::new(meshes, materials, nodes, roots)
        .with_skins(skins)
        .with_animations(animations)
        .with_variants(variants)
        .with_extras(import_extras(&gltf)))
}

/// 读 `KHR_texture_transform`：贴图的偏移 / 旋转 / 缩放。
///
/// # 引擎这边只有一套 UV 变换
///
/// [`kpbr::standard::UV_SCALE`] / `UV_OFFSET` 是**逐材质**的，所有贴图槽
/// 共用——这是有意的，否则法线贴图和基础色会错位。而
/// `KHR_texture_transform` 是**逐贴图**的，同一个材质里的两张贴图可以各
/// 变各的。
///
/// 取舍：只认基础色贴图上的那份变换，用它设整个材质的 UV 变换。
/// 绝大多数导出器（Blender、gltfpack）把同一个变换写给材质里的每一张图，
/// 所以这个近似在实际文件上是准确的；真的每张图各变各的时候，
/// 非基础色的那几张会错位。
///
/// **旋转不支持**：引擎的 UV 变换是 `uv * scale + offset`，没有旋转项。
/// 文件里写了非零旋转时记一条警告，而不是静默忽略——贴图转没转是
/// 一眼能看出来的，但「为什么没转」不写下来就没人知道。
fn apply_texture_transform(source: &gltf::Material<'_>, material: &mut Material) {
    let Some(transform) = source
        .pbr_metallic_roughness()
        .base_color_texture()
        .and_then(|info| info.extension_value("KHR_texture_transform").cloned())
    else {
        return;
    };
    let pair = |key: &str, default: [f32; 2]| -> [f32; 2] {
        transform
            .get(key)
            .and_then(|v| v.as_array())
            .filter(|a| a.len() == 2)
            .map_or(default, |a| {
                [
                    a[0].as_f64().unwrap_or(default[0] as f64) as f32,
                    a[1].as_f64().unwrap_or(default[1] as f64) as f32,
                ]
            })
    };
    let scale = pair("scale", [1.0, 1.0]);
    let offset = pair("offset", [0.0, 0.0]);
    let rotation = transform
        .get("rotation")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if rotation.abs() > 1e-6 {
        klog::warn!(
            "KHR_texture_transform 的 rotation（{rotation:.3} 弧度）被忽略：引擎的 UV 变换没有旋转项"
        );
    }
    material.set(kpbr::standard::UV_SCALE, kmath::Vec2::from_array(scale));
    material.set(kpbr::standard::UV_OFFSET, kmath::Vec2::from_array(offset));
}

/// 读 `KHR_materials_variants`。
///
/// 扩展分两半：文档根上是变体的**名单**，每个图元上是「我在第 i 个变体下
/// 用哪个材质」的映射。这里把它翻成「(节点, 部件) → 材质」的覆盖表，
/// 因为引擎的场景里能改的是**节点上的材质**，没有「图元」这一层。
///
/// 节点和部件的对应关系要用已经建好的 `nodes`：一个 glTF mesh 可能有
/// 好几个图元，导入之后成了同一个节点的好几个 part，序号是一一对应的。
fn import_variants(gltf: &gltf::Gltf, nodes: &[ModelNode]) -> Vec<crate::Variant> {
    let Some(names) = gltf
        .extension_value("KHR_materials_variants")
        .and_then(|value| value.get("variants"))
        .and_then(|value| value.as_array())
    else {
        return Vec::new();
    };
    let mut variants: Vec<crate::Variant> = names
        .iter()
        .enumerate()
        .map(|(index, entry)| crate::Variant {
            name: entry
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("variant{index}"))
                .to_string(),
            overrides: Vec::new(),
        })
        .collect();

    for source in gltf.nodes() {
        let index = source.index();
        if index >= nodes.len() {
            continue;
        }
        let Some(mesh) = source.mesh() else { continue };
        for (part, primitive) in mesh.primitives().enumerate() {
            // 导入时跳过了非三角形图元，序号会对不上。这批模型没有那种
            // 图元，真遇到时宁可少换一个材质，也不要换错一个。
            if part >= nodes[index].parts.len() {
                break;
            }
            let Some(mappings) = primitive
                .extension_value("KHR_materials_variants")
                .and_then(|value| value.get("mappings"))
                .and_then(|value| value.as_array())
            else {
                continue;
            };
            for mapping in mappings {
                let Some(material) = mapping.get("material").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let Some(list) = mapping.get("variants").and_then(|v| v.as_array()) else {
                    continue;
                };
                for slot in list.iter().filter_map(|v| v.as_u64()) {
                    if let Some(variant) = variants.get_mut(slot as usize) {
                        variant.overrides.push((index, part, material as usize));
                    }
                }
            }
        }
    }
    variants
}

/// 把文件里挂着的 `extras` 原样收起来。
///
/// 不解析：规范只说它是一段任意 JSON，里面有什么全看导出它的工具
/// （Blender 的自定义属性、关卡编辑器的标记……）。引擎替谁猜结构都是错的，
/// 原文交给游戏自己解释。
fn import_extras(gltf: &gltf::Gltf) -> GltfExtras {
    /// `gltf` crate 把 extras 存成 `Option<Box<RawValue>>`，取原文即可。
    fn text(extras: &gltf::json::Extras) -> Option<String> {
        extras.as_ref().map(|raw| raw.get().to_string())
    }

    GltfExtras {
        // 默认场景的 extras。没有默认场景时退回第一个——glTF 允许不指定，
        // 而查看器一律显示第一个，这里跟着它走。
        scene: gltf
            .default_scene()
            .or_else(|| gltf.scenes().next())
            .and_then(|scene| text(scene.extras())),
        nodes: gltf.nodes().map(|node| text(node.extras())).collect(),
        meshes: gltf.meshes().map(|mesh| text(mesh.extras())).collect(),
        materials: gltf
            .materials()
            .map(|material| text(material.extras()))
            .collect(),
    }
}

/// 从 `mesh.extras` 里读形变目标的名字。
///
/// 这是 glTF 的约定俗成而非规范强制，读不到就返回空——调用方会退回占位名。
fn read_target_names(mesh: &gltf::Mesh<'_>) -> Vec<String> {
    let Some(extras) = mesh.extras().as_ref() else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(extras.get()) else {
        return Vec::new();
    };

    value
        .get("targetNames")
        .and_then(serde_json::Value::as_array)
        .map(|names| {
            names
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 读一个图元的全部形变目标。
///
/// glTF 允许目标只带位置不带法线，这时法线增量按零处理——
/// 形状变了而法线不变，光照会有点假，但总比没有形变强。
fn read_morph_targets<'a, F>(
    primitive: &gltf::Primitive<'_>,
    reader: &gltf::mesh::Reader<'a, 'a, F>,
    vertex_count: usize,
    names: &[String],
) -> Vec<MorphTarget>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'a [u8]>,
{
    if primitive.morph_targets().len() == 0 {
        return Vec::new();
    }

    reader
        .read_morph_targets()
        .enumerate()
        .map(|(index, (positions, normals, _tangents))| {
            let positions: Vec<[f32; 3]> = positions.map(Iterator::collect).unwrap_or_default();
            let normals: Vec<[f32; 3]> = normals.map(Iterator::collect).unwrap_or_default();

            let deltas: Vec<MorphDelta> = (0..vertex_count)
                .map(|vertex| MorphDelta {
                    position: positions.get(vertex).copied().unwrap_or([0.0; 3]),
                    normal: normals.get(vertex).copied().unwrap_or([0.0; 3]),
                    ..Default::default()
                })
                .collect();

            let name = names
                .get(index)
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("Target{index}"));
            MorphTarget::new(name, deltas)
        })
        .collect()
}

/// 导入骨架：关节列表与逆绑定矩阵。
fn import_skins(gltf: &gltf::Gltf, buffers: &[Vec<u8>]) -> Vec<ModelSkin> {
    gltf.skins()
        .map(|skin| {
            let joints: Vec<usize> = skin.joints().map(|joint| joint.index()).collect();

            let reader = skin.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
            let mut inverse_bind: Vec<Mat4> = reader
                .read_inverse_bind_matrices()
                .map(|matrices| matrices.map(|m| Mat4::from_cols_array_2d(&m)).collect())
                .unwrap_or_default();

            // glTF 允许省略逆绑定矩阵，此时按单位矩阵处理；
            // 数量不足也补齐，免得后面按关节号取矩阵时越界。
            inverse_bind.resize(joints.len(), Mat4::IDENTITY);

            ModelSkin {
                joints,
                inverse_bind,
                skeleton: skin.skeleton().map(|node| node.index()),
            }
        })
        .collect()
}

/// 导入动画剪辑。
///
/// glTF 的一个 channel 正好对应一条轨道：目标是节点，路径是 TRS 之一。
/// 目标用**节点序号**而不是名字——名字可以重复，也可以为空。
fn import_animations(gltf: &gltf::Gltf, buffers: &[Vec<u8>]) -> Vec<AnimationClip> {
    gltf.animations()
        .enumerate()
        .map(|(index, animation)| {
            let name = animation
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("Animation{index}"));

            let mut tracks = Vec::new();
            for channel in animation.channels() {
                let target = channel.target().node().index();
                let interpolation = match channel.sampler().interpolation() {
                    gltf::animation::Interpolation::Linear => Interpolation::Linear,
                    gltf::animation::Interpolation::Step => Interpolation::Step,
                    gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
                };

                let reader =
                    channel.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
                let Some(times) = reader.read_inputs() else {
                    klog::warn!("动画 {name} 的某个通道缺少时间轴，已跳过");
                    continue;
                };
                let times: Vec<f32> = times.collect();
                let Some(outputs) = reader.read_outputs() else {
                    klog::warn!("动画 {name} 的某个通道缺少采样值，已跳过");
                    continue;
                };

                let channel = match outputs {
                    gltf::animation::util::ReadOutputs::Translations(values) => {
                        Curve::new(times, values.map(Vec3::from_array).collect(), interpolation)
                            .map(Channel::Position)
                    }
                    gltf::animation::util::ReadOutputs::Scales(values) => {
                        Curve::new(times, values.map(Vec3::from_array).collect(), interpolation)
                            .map(Channel::Scale)
                    }
                    gltf::animation::util::ReadOutputs::Rotations(values) => Curve::new(
                        times,
                        values
                            .into_f32()
                            // glTF 的四元数是 [x, y, z, w] 顺序。
                            .map(|q| Quat::from_xyzw(q[0], q[1], q[2], q[3]))
                            .collect(),
                        interpolation,
                    )
                    .map(Channel::Rotation),
                    gltf::animation::util::ReadOutputs::MorphTargetWeights(values) => {
                        // 一个 weights 通道同时驱动 N 个形变目标，采样值是交错排列的。
                        // 这里拆成 N 条标量轨道：曲线的值类型因此保持定长，
                        // 混合逻辑也不用为「变长数组」单开一套。
                        let values: Vec<f32> = values.into_f32().collect();
                        let frames = times.len();
                        // 三次样条每帧存三份（n 个入切线、n 个值、n 个出切线），
                        // 算目标个数时要把这三份除掉。
                        let per_frame = if interpolation == Interpolation::CubicSpline {
                            3
                        } else {
                            1
                        };
                        let count = values.len() / (frames * per_frame).max(1);

                        for slot in 0..count {
                            let weights: Vec<f32> = (0..frames)
                                .flat_map(|frame| {
                                    let base = frame * count * per_frame;
                                    if per_frame == 3 {
                                        // 拆出这个目标的「入切线, 值, 出切线」三元组。
                                        vec![
                                            values[base + slot],
                                            values[base + count + slot],
                                            values[base + count * 2 + slot],
                                        ]
                                    } else {
                                        vec![values[base + slot]]
                                    }
                                })
                                .collect();

                            if let Some(curve) = Curve::new(times.clone(), weights, interpolation) {
                                tracks.push(Track {
                                    target,
                                    channel: Channel::MorphWeight { index: slot, curve },
                                });
                            }
                        }
                        None
                    }
                };

                match channel {
                    Some(channel) => tracks.push(Track { target, channel }),
                    None => continue,
                }
            }

            AnimationClip::new(name, tracks)
        })
        .collect()
}

/// 加载全部缓冲区。GLB 的内嵌 BIN 块、data URI 与外部 .bin 都在这里统一。
async fn load_buffers(
    gltf: &gltf::Gltf,
    base: &Path,
    io: &Arc<dyn ResourceIo>,
) -> Result<Vec<Vec<u8>>, LoadError> {
    let mut buffers = Vec::with_capacity(gltf.buffers().len());

    for buffer in gltf.buffers() {
        let data = match buffer.source() {
            gltf::buffer::Source::Bin => gltf
                .blob
                .clone()
                .ok_or_else(|| LoadError::message("glTF 引用了 BIN 块，但文件里没有"))?,
            gltf::buffer::Source::Uri(source) => uri::read_uri(source, base, io).await?,
        };

        // 缓冲允许多出几个字节做对齐，但不能少。
        if data.len() < buffer.length() {
            return Err(LoadError::message(format!(
                "缓冲区长度不足：声明 {} 字节，实际 {} 字节",
                buffer.length(),
                data.len()
            )));
        }

        buffers.push(data);
    }

    Ok(buffers)
}

/// 加载全部贴图。单张贴图失败不影响整个模型，记日志后跳过。
async fn load_textures(
    gltf: &gltf::Gltf,
    base: &Path,
    io: &Arc<dyn ResourceIo>,
    buffers: &[Vec<u8>],
    model_path: &Path,
) -> Vec<Option<Resource<Texture>>> {
    let mut textures = Vec::with_capacity(gltf.images().len());

    for image in gltf.images() {
        let bytes = match image.source() {
            gltf::image::Source::Uri { uri: source, .. } => {
                match uri::read_uri(source, base, io).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        klog::warn!("贴图加载失败（{source}）：{error}");
                        textures.push(None);
                        continue;
                    }
                }
            }
            gltf::image::Source::View { view, .. } => {
                let Some(buffer) = buffers.get(view.buffer().index()) else {
                    klog::warn!("贴图引用了不存在的缓冲区");
                    textures.push(None);
                    continue;
                };
                let start = view.offset();
                let end = start + view.length();
                if end > buffer.len() {
                    klog::warn!("贴图的缓冲区视图越界");
                    textures.push(None);
                    continue;
                }
                buffer[start..end].to_vec()
            }
        };

        match Texture::from_encoded(&bytes) {
            Ok(texture) => {
                // 贴图不是独立文件时也需要一个路径做资源键，用模型路径加索引区分。
                let key = format!("{}#image{}", model_path.display(), image.index());
                textures.push(Some(Resource::new_ok(key, texture)));
            }
            Err(error) => {
                klog::warn!("贴图解码失败：{error}");
                textures.push(None);
            }
        }
    }

    textures
}

fn import_materials(gltf: &gltf::Gltf, textures: &[Option<Resource<Texture>>]) -> Vec<Material> {
    // One image may be both sRGB color and linear data, or use different samplers.
    // Cache each texture/role separately; Texture clones share immutable pixel storage.
    let mut cache = std::collections::HashMap::new();
    let mut texture_for = |source: gltf::Texture<'_>, linear: bool| -> Option<Resource<Texture>> {
        let key = (source.index(), linear);
        if let Some(texture) = cache.get(&key) { return Some(Resource::clone(texture)); }
        let resource = textures.get(source.source().index())?.as_ref()?;
        let image = resource.data_ref()?;
        let wrap = |mode| match mode {
            gltf::texture::WrappingMode::ClampToEdge => ktexture::WrapMode::ClampToEdge,
            gltf::texture::WrappingMode::MirroredRepeat => ktexture::WrapMode::MirrorRepeat,
            gltf::texture::WrappingMode::Repeat => ktexture::WrapMode::Repeat,
        };
        let s = source.sampler();
        let sampler = ktexture::Sampler {
            wrap_u: wrap(s.wrap_s()), wrap_v: wrap(s.wrap_t()),
            mag_filter: if s.mag_filter()==Some(gltf::texture::MagFilter::Nearest) {ktexture::FilterMode::Nearest}else{ktexture::FilterMode::Linear},
            min_filter: if matches!(s.min_filter(), Some(gltf::texture::MinFilter::Nearest | gltf::texture::MinFilter::NearestMipmapNearest | gltf::texture::MinFilter::NearestMipmapLinear)) {ktexture::FilterMode::Nearest}else{ktexture::FilterMode::Linear},
        };
        let image = (*image).clone().with_format(if linear {ktexture::TextureFormat::Linear}else{ktexture::TextureFormat::Srgb}).with_sampler(sampler);
        let resource = Resource::new_ok(format!("gltf#texture{}:{linear}", source.index()), image);
        cache.insert(key,resource.clone());
        Some(resource)
    };
    gltf.materials()
        .map(|source| {
            let pbr = source.pbr_metallic_roughness();
            let color = pbr.base_color_factor();

            let mut material = Material::standard()
                .with_base_color(Vec4::from_array(color))
                .with_metallic(pbr.metallic_factor())
                .with_roughness(pbr.roughness_factor());

            if let Some(info) = pbr.base_color_texture() {
                if let Some(texture) = texture_for(info.texture(), false) {
                    material = material.with_base_color_texture(texture);
                }
            }
            if let Some(info) = pbr.metallic_roughness_texture() && let Some(texture) = texture_for(info.texture(), true) {
                material.set(kpbr::standard::METALLIC_ROUGHNESS_TEXTURE, texture);
            }
            if let Some(info) = source.normal_texture() && let Some(texture) = texture_for(info.texture(), true) {
                material.set(kpbr::standard::NORMAL_TEXTURE, texture);
                material.set("normal_scale", info.scale());
            }
            if let Some(info) = source.occlusion_texture() && let Some(texture) = texture_for(info.texture(), true) {
                material.set(kpbr::standard::OCCLUSION_TEXTURE, texture);
                material.set(kpbr::standard::OCCLUSION, info.strength());
            }
            if let Some(info) = source.emissive_texture() && let Some(texture) = texture_for(info.texture(), false) {
                material.set(kpbr::standard::EMISSIVE_TEXTURE, texture);
            }
            material.set(kpbr::standard::EMISSIVE, Vec3::from_array(source.emissive_factor()) * source.emissive_strength().unwrap_or(1.0));
            apply_texture_transform(&source, &mut material);
            material.set_double_sided(source.double_sided());
            if source.alpha_mode()==gltf::material::AlphaMode::Blend { material.set_blend_mode(kmaterial::BlendMode::Alpha); }
            let mut physical = kpbr::physical::Physical::default();
            physical.unlit=source.unlit();
            physical.alpha_cutoff=if source.alpha_mode()==gltf::material::AlphaMode::Mask {source.alpha_cutoff().unwrap_or(0.5)}else{0.0};
            physical.transmission=source.transmission().map_or(0.0,|t|t.transmission_factor());
            physical.ior=source.ior().unwrap_or(1.5);
            physical.thickness=source.volume().map_or(0.0,|v|v.thickness_factor());
            let number=|extension:&str,key:&str,default:f32|source.extension_value(extension).and_then(|e|e.get(key)).and_then(|v|v.as_f64()).map_or(default,|v|v as f32);
            physical.dispersion=number("KHR_materials_dispersion","dispersion",0.0);
            physical.iridescence=number("KHR_materials_iridescence","iridescenceFactor",0.0);
            physical.film_thickness=number("KHR_materials_iridescence","iridescenceThicknessMaximum",400.0);
            physical.sheen_roughness=number("KHR_materials_sheen","sheenRoughnessFactor",0.0);
            if let Some(color)=source.extension_value("KHR_materials_sheen").and_then(|e|e.get("sheenColorFactor")).and_then(|v|v.as_array()) && color.len()==3 {
                physical.sheen=Vec3::new(color[0].as_f64().unwrap_or(0.0) as f32,color[1].as_f64().unwrap_or(0.0) as f32,color[2].as_f64().unwrap_or(0.0) as f32);
            }
            if physical.transmission>0.0 || physical.iridescence>0.0 || physical.sheen!=Vec3::ZERO || physical.unlit || physical.alpha_cutoff>0.0 {physical.apply(&mut material);}

            // 名字留着：游戏侧要按「LeatherPartsMat」这种美术起的名字找到
            // 具体某一块去改颜色。用序号找的话，美术重新导出一次就全错位了。
            if let Some(name) = source.name() {
                material.set_name(name);
            }

            material
        })
        .collect()
}

/// 导入所有网格。
///
/// glTF 的一个 mesh 含多个 primitive，而引擎的 [`Mesh`] 对应单个 primitive，
/// 因此返回值第二项记录了每个 glTF mesh 展开成了哪几块。
fn import_meshes(
    gltf: &gltf::Gltf,
    buffers: &[Vec<u8>],
    path: &std::path::Path,
) -> Result<(Vec<Mesh>, Vec<Vec<MeshPart>>), LoadError> {
    let mut meshes = Vec::new();
    let mut ranges = Vec::with_capacity(gltf.meshes().len());

    for source in gltf.meshes() {
        let mut parts = Vec::new();
        // 形变目标的名字藏在 mesh.extras 的 targetNames 里——
        // glTF 规范没给它专门的字段，只能自己从扩展数据里挖。
        let target_names = read_target_names(&source);
        let default_weights: Vec<f32> = source.weights().map(<[f32]>::to_vec).unwrap_or_default();

        for primitive in source.primitives() {
            // 只处理三角形；点、线等模式暂不支持。
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                klog::warn!("跳过非三角形图元：{:?}", primitive.mode());
                continue;
            }

            let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));

            let Some(positions) = reader.read_positions() else {
                klog::warn!("图元缺少 POSITION 属性，已跳过");
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();

            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|tc| tc.into_f32().collect());
            // 第二套 UV（`TEXCOORD_1`）：lightmap 用的那一套，没有就留零——
            // 零对没读 `uv1` 的着色器/材质没有任何影响。
            let uv1s: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(1).map(|tc| tc.into_f32().collect());
            let colors: Option<Vec<[f32; 4]>> =
                reader.read_colors(0).map(|c| c.into_rgba_f32().collect());
            let tangents: Option<Vec<[f32; 4]>> = reader.read_tangents().map(Iterator::collect);

            let vertices: Vec<Vertex> = positions
                .iter()
                .enumerate()
                .map(|(index, position)| Vertex {
                    position: *position,
                    normal: normals
                        .as_ref()
                        .and_then(|n| n.get(index).copied())
                        .unwrap_or([0.0, 1.0, 0.0]),
                    uv: uvs
                        .as_ref()
                        .and_then(|u| u.get(index).copied())
                        .unwrap_or([0.0, 0.0]),
                    color: colors
                        .as_ref()
                        .and_then(|c| c.get(index).copied())
                        .map(|c| [c[0], c[1], c[2]])
                        .unwrap_or([1.0; 3]),
                    tangent: tangents
                        .as_ref()
                        .and_then(|t| t.get(index).copied())
                        .unwrap_or([1.0, 0.0, 0.0, 1.0]),
                    uv1: uv1s
                        .as_ref()
                        .and_then(|u| u.get(index).copied())
                        .unwrap_or([0.0, 0.0]),
                })
                .collect();

            // 蒙皮属性：两个都在才算数，缺一个就当静态网格处理。
            let joints: Option<Vec<[u16; 4]>> =
                reader.read_joints(0).map(|j| j.into_u16().collect());
            let weights: Option<Vec<[f32; 4]>> =
                reader.read_weights(0).map(|w| w.into_f32().collect());

            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                // 没有索引缓冲时按顶点顺序组成三角形。
                None => (0..vertices.len() as u32).collect(),
            };

            let mut mesh = Mesh::new(vertices, indices);
            if !mesh.is_valid() {
                klog::warn!("图元的索引数据非法，已跳过");
                continue;
            }
            // glTF 允许省略法线，此时需要自行生成，否则光照全黑。
            if normals.is_none() {
                mesh.recompute_normals();
            }
            // 同样，缺少 TANGENT 时法线贴图无法工作，按 UV 反推一份。
            if tangents.is_none() {
                mesh.recompute_tangents();
            }

            // ── 形变目标 ──
            let morph_targets =
                read_morph_targets(&primitive, &reader, mesh.vertices().len(), &target_names);
            if !morph_targets.is_empty() {
                mesh = mesh.with_morph_targets(morph_targets, default_weights.clone());
            }

            if let (Some(joints), Some(weights)) = (joints, weights) {
                let skin: Vec<SkinVertex> = (0..mesh.vertices().len())
                    .map(|index| SkinVertex {
                        joints: joints.get(index).copied().unwrap_or([0; 4]),
                        weights: weights.get(index).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]),
                    })
                    .collect();
                // 权重的归一化交给 `with_skin`，它对所有来源一视同仁。
                mesh = mesh.with_skin(skin);
            }

            // 打上出处：场景序列化时凭它写一行引用，而不是把几何整个抄一遍。
            let mesh = mesh.with_source(kmesh::MeshSource::new(path, meshes.len()));

            parts.push(MeshPart {
                mesh: meshes.len(),
                material: primitive.material().index(),
            });
            meshes.push(mesh);
        }

        ranges.push(parts);
    }

    Ok((meshes, ranges))
}

fn import_nodes(
    gltf: &gltf::Gltf,
    primitive_ranges: &[Vec<MeshPart>],
) -> (Vec<ModelNode>, Vec<usize>) {
    let nodes: Vec<ModelNode> = gltf
        .nodes()
        .map(|source| {
            let (position, rotation, scale) = source.transform().decomposed();

            ModelNode {
                name: source.name().unwrap_or_default().to_string(),
                transform: NodeTransform {
                    position: Vec3::from_array(position),
                    // glTF 的四元数是 [x, y, z, w] 顺序。
                    rotation: Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
                    scale: Vec3::from_array(scale),
                },
                children: source.children().map(|child| child.index()).collect(),
                parts: source
                    .mesh()
                    .and_then(|mesh| primitive_ranges.get(mesh.index()))
                    .cloned()
                    .unwrap_or_default(),
                skin: source.skin().map(|skin| skin.index()),
            }
        })
        .collect();

    // 优先用默认场景的根节点；没有场景定义时，把所有非子节点当作根。
    let roots = match gltf.default_scene().or_else(|| gltf.scenes().next()) {
        Some(scene) => scene.nodes().map(|node| node.index()).collect(),
        None => {
            let mut is_child = vec![false; nodes.len()];
            for node in &nodes {
                for &child in &node.children {
                    if let Some(flag) = is_child.get_mut(child) {
                        *flag = true;
                    }
                }
            }
            is_child
                .iter()
                .enumerate()
                .filter(|(_, child)| !**child)
                .map(|(index, _)| index)
                .collect()
        }
    };

    (nodes, roots)
}

/// Expand glTF GPU instance transforms into nodes sharing one Mesh id. The
/// renderer batches those shared meshes back into GPU instanced draws.
fn import_instances(gltf:&gltf::Gltf,buffers:&[Vec<u8>],nodes:&mut Vec<ModelNode>)->Result<(),LoadError>{
    for source in gltf.nodes(){
        let Some(extension)=source.extension_value("EXT_mesh_gpu_instancing") else {continue};
        let attrs=extension.get("attributes").and_then(|v|v.as_object()).ok_or_else(||LoadError::message("instancing attributes missing"))?;
        if source.skin().is_some(){return Err(LoadError::message("skinned EXT_mesh_gpu_instancing is not supported"));}
        let accessor=|key:&str|->Result<Option<gltf::Accessor<'_>>,LoadError>{
            let Some(value)=attrs.get(key) else{return Ok(None)};
            let index=value.as_u64().ok_or_else(||LoadError::message("invalid instance accessor index"))? as usize;
            Ok(Some(gltf.accessors().nth(index).ok_or_else(||LoadError::message("instance accessor out of range"))?))
        };
        let read3=|key:&str|->Result<Option<Vec<[f32;3]>>,LoadError>{
            accessor(key)?.map(|a|gltf::accessor::Iter::<[f32;3]>::new(a,|b|buffers.get(b.index()).map(Vec::as_slice)).map(Iterator::collect).ok_or_else(||LoadError::message("invalid VEC3 instance data"))).transpose()
        };
        let positions=read3("TRANSLATION")?;let scales=read3("SCALE")?;
        let rotations=accessor("ROTATION")?.map(|a|gltf::accessor::Iter::<[f32;4]>::new(a,|b|buffers.get(b.index()).map(Vec::as_slice)).map(Iterator::collect::<Vec<_>>).ok_or_else(||LoadError::message("invalid VEC4 instance rotation"))).transpose()?;
        let counts=[positions.as_ref().map(Vec::len),scales.as_ref().map(Vec::len),rotations.as_ref().map(Vec::len)];
        let count=counts.into_iter().flatten().next().ok_or_else(||LoadError::message("instancing has no TRS attributes"))?;
        if count==0 || count>100_000 || counts.into_iter().flatten().any(|n|n!=count){return Err(LoadError::message("instance counts mismatch or exceed limit"));}
        let original=source.index();let parts=std::mem::take(&mut nodes[original].parts);
        for instance in 0..count{
            let position=positions.as_ref().map_or(Vec3::ZERO,|p|Vec3::from_array(p[instance]));
            let scale=scales.as_ref().map_or(Vec3::ONE,|p|Vec3::from_array(p[instance]));
            let rotation=rotations.as_ref().map_or(Quat::IDENTITY,|p|Quat::from_array(p[instance]));
            if !position.is_finite()||!scale.is_finite()||!rotation.is_finite()||rotation.length_squared()<1e-8 {return Err(LoadError::message("non-finite instance transform"));}
            let child=nodes.len();nodes[original].children.push(child);
            nodes.push(ModelNode{name:format!("Instance{instance}"),transform:NodeTransform{position,scale,rotation:rotation.normalize()},parts:parts.clone(),..Default::default()});
        }
    }
    Ok(())
}
