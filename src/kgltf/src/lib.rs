//! kgltf —— glTF 2.0 模型导入。
//!
//! 产出的 [`Model`] 是中立的数据描述（网格 + 材质 + 节点树），
//! 不依赖引擎的场景类型；由引擎负责把它实例化成场景节点。
//!
//! # 支持范围
//!
//! - `.gltf`（含 `data:` 内嵌缓冲）与 `.glb`
//! - 外部 `.bin` 与外部贴图，通过 [`ResourceIo`](kasset::ResourceIo) 读取
//! - 三角形图元、PBR 基础色/金属度/粗糙度、基础色贴图、节点层级
//! - 顶点切线（缺失 TANGENT 时按 UV 反推生成）
//!
//! 尚未支持：动画、蒙皮、非三角形图元、稀疏访问器。
//! 法线/遮蔽/自发光贴图渲染器已能消费，但导入器目前只接基础色贴图。
//!
//! ```no_run
//! use kgltf::prelude::*;
//! use kasset::ResourceManager;
//!
//! let manager = ResourceManager::new();
//! manager.add_loader(GltfLoader);
//!
//! let model: Resource<Model> = manager.request("assets/duck.glb");
//! ```

#![warn(missing_docs)]

mod importer;
mod loader;
mod model;
mod uri;

pub use kanim::AnimationClip;
pub use loader::GltfLoader;
pub use model::{
    GltfExtras, MODEL_TYPE_UUID, MeshPart, Model, ModelNode, ModelSkin, NodeTransform,
};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{GltfLoader, MeshPart, Model, ModelNode, NodeTransform};
    pub use kasset::Resource;
}

#[cfg(test)]
mod test {
    use crate::prelude::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use kasset::{MemoryResourceIo, ResourceManager};
    use kmath::Vec3;
    use std::sync::Arc;

    /// 构造一个自包含的最小 glTF：单个三角形，缓冲以 base64 内嵌。
    fn triangle_gltf(extra_node: bool) -> String {
        let mut buffer = Vec::new();
        // 3 个 vec3 位置，共 36 字节
        for position in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for component in position {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        // 3 个 u16 索引，共 6 字节
        for index in [0u16, 1, 2] {
            buffer.extend_from_slice(&index.to_le_bytes());
        }
        let encoded = STANDARD.encode(&buffer);
        let byte_length = buffer.len();

        let nodes = if extra_node {
            r#"{"mesh":0,"name":"Triangle","children":[1]},{"name":"Child","translation":[0.0,2.0,0.0]}"#
        } else {
            r#"{"mesh":0,"name":"Triangle"}"#
        };

        format!(
            r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{nodes}],
  "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}, "indices": 1, "material": 0}}]}}],
  "materials": [{{"pbrMetallicRoughness": {{
      "baseColorFactor": [1.0, 0.0, 0.0, 1.0],
      "metallicFactor": 0.25,
      "roughnessFactor": 0.75
  }}}}],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
     "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]}},
    {{"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}}
  ],
  "bufferViews": [
    {{"buffer": 0, "byteOffset": 0, "byteLength": 36, "target": 34962}},
    {{"buffer": 0, "byteOffset": 36, "byteLength": 6, "target": 34963}}
  ],
  "buffers": [{{"byteLength": {byte_length},
    "uri": "data:application/octet-stream;base64,{encoded}"}}]
}}"#
        )
    }

    fn load(source: &str, path: &str) -> Result<Resource<Model>, kasset::LoadError> {
        let mut io = MemoryResourceIo::new();
        io.add(path, source);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(GltfLoader);
        manager.request_blocking::<Model>(path)
    }

    #[test]
    fn imports_triangle_geometry() {
        let model = load(&triangle_gltf(false), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();

        assert_eq!(model.meshes().len(), 1);
        assert_eq!(model.triangle_count(), 1);

        let mesh = model.mesh(0).unwrap();
        assert_eq!(mesh.vertices().len(), 3);
        assert_eq!(mesh.indices(), &[0, 1, 2]);
        assert_eq!(mesh.vertices()[1].position(), Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn generates_normals_when_absent() {
        // 这个 glTF 没有 NORMAL 属性，导入器必须自行生成，
        // 否则模型在光照下会全黑。
        let model = load(&triangle_gltf(false), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();
        let mesh = model.mesh(0).unwrap();

        // 三角形位于 XY 平面，法线应指向 ±Z。
        for vertex in mesh.vertices() {
            assert!((vertex.normal().length() - 1.0).abs() < 1e-5);
            assert!(vertex.normal().z.abs() > 0.99);
        }
    }

    #[test]
    fn generates_tangents_when_absent() {
        // 测试用的 glTF 没有 TANGENT 属性，导入器必须自行生成，
        // 否则挂上法线贴图后光照方向会完全错乱。
        let model = load(&triangle_gltf(false), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();
        let mesh = model.mesh(0).unwrap();

        for vertex in mesh.vertices() {
            assert!(vertex.tangent().is_finite());
            // 切线必须垂直于法线。
            assert!(vertex.normal().dot(vertex.tangent()).abs() < 1e-3);
        }
    }

    #[test]
    fn imports_pbr_material() {
        let model = load(&triangle_gltf(false), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();

        let material = model.material(0).expect("材质应当存在");
        assert_eq!(material.base_color().x, 1.0);
        assert_eq!(material.base_color().y, 0.0);
        assert_eq!(material.metallic(), 0.25);
        assert_eq!(material.roughness(), 0.75);
    }

    #[test]
    fn imports_node_hierarchy() {
        let model = load(&triangle_gltf(true), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();

        assert_eq!(model.roots(), &[0]);

        let root = model.node(0).unwrap();
        assert_eq!(root.name, "Triangle");
        assert_eq!(root.children, vec![1]);
        assert_eq!(root.parts.len(), 1);
        assert_eq!(root.parts[0].material, Some(0));

        let child = model.node(1).unwrap();
        assert_eq!(child.transform.position, Vec3::new(0.0, 2.0, 0.0));
        // 子节点没有网格。
        assert!(child.parts.is_empty());
    }

    #[test]
    fn malformed_gltf_reports_error() {
        let error = load("{ not valid json", "bad.gltf").expect_err("非法 glTF 应当加载失败");

        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn missing_buffer_file_reports_error() {
        // 引用了一个外部 .bin，但内存文件系统里没有它。
        let source = r#"{
          "asset": {"version": "2.0"},
          "scenes": [{"nodes": []}],
          "buffers": [{"byteLength": 4, "uri": "missing.bin"}]
        }"#;

        assert!(load(source, "m.gltf").is_err());
    }

    #[test]
    fn loads_external_buffer_through_io() {
        // 缓冲放在独立的 .bin 里，验证相对路径解析与 ResourceIo 读取。
        let mut buffer = Vec::new();
        for position in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for component in position {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        for index in [0u16, 1, 2] {
            buffer.extend_from_slice(&index.to_le_bytes());
        }

        let source = r#"{
          "asset": {"version": "2.0"},
          "scene": 0,
          "scenes": [{"nodes": [0]}],
          "nodes": [{"mesh": 0}],
          "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
          "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0,0.0,0.0], "max": [1.0,1.0,0.0]},
            {"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}
          ],
          "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 6}
          ],
          "buffers": [{"byteLength": 42, "uri": "geometry.bin"}]
        }"#;

        let mut io = MemoryResourceIo::new();
        io.add("models/scene.gltf", source);
        io.add("models/geometry.bin", buffer);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(GltfLoader);

        let model = manager
            .request_blocking::<Model>("models/scene.gltf")
            .expect("外部缓冲应当能加载");

        assert_eq!(model.data_ref().unwrap().triangle_count(), 1);
    }

    #[test]
    fn primitive_without_material_is_allowed() {
        let source = triangle_gltf(false).replace(r#", "material": 0"#, "");
        let model = load(&source, "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();

        assert_eq!(model.node(0).unwrap().parts[0].material, None);
    }

    /// 加载仓库里的 Soldier.glb —— 一个真实的骨骼动画模型
    /// （49 关节 + 4 个动画）。合成的测试数据覆盖不到真实导出器的种种细节，
    /// 这个文件是骨骼动画那条链路唯一的可信验证。
    fn soldier() -> Option<Resource<Model>> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/Soldier.glb");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("跳过依赖 assets/Soldier.glb 的测试：文件不存在");
            return None;
        };

        let mut io = MemoryResourceIo::new();
        io.add("Soldier.glb", bytes);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(GltfLoader);
        Some(
            manager
                .request_blocking::<Model>("Soldier.glb")
                .expect("Soldier.glb 应当能加载"),
        )
    }

    #[test]
    fn soldier_imports_its_skins() {
        let Some(model) = soldier() else { return };
        let model = model.data_ref().unwrap();

        // 文件里有两副骨架：身体 49 个关节，面罩 2 个。
        assert_eq!(model.skins().len(), 2);
        let body = &model.skins()[0];
        assert_eq!(body.joints.len(), 49);
        // 逆绑定矩阵必须与关节一一对应，否则蒙皮会整个错位。
        assert_eq!(body.inverse_bind.len(), body.joints.len());
        assert!(body.inverse_bind.iter().all(|m| m.is_finite()));
        // 关节指向的节点都要真实存在。
        assert!(body.joints.iter().all(|&joint| joint < model.nodes().len()));
    }

    #[test]
    fn soldier_imports_its_animations() {
        let Some(model) = soldier() else { return };
        let model = model.data_ref().unwrap();

        let names: Vec<&str> = model.animations().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["Idle", "Run", "TPose", "Walk"]);
        assert!(model.is_animated());

        for clip in model.animations().iter() {
            // 每个剪辑 156 条通道，时长为正。
            assert_eq!(clip.tracks().len(), 156);
            assert!(clip.duration() > 0.0, "{} 的时长为 0", clip.name());
            // 轨道的目标必须是真实节点。
            assert!(
                clip.tracks().iter().all(|t| t.target < model.nodes().len()),
                "{} 有指向不存在节点的轨道",
                clip.name()
            );
        }
    }

    #[test]
    fn soldier_meshes_carry_skin_attributes() {
        let Some(model) = soldier() else { return };
        let model = model.data_ref().unwrap();

        assert!(!model.meshes().is_empty());
        for mesh in model.meshes() {
            assert!(mesh.is_skinned(), "蒙皮网格没读到 JOINTS_0/WEIGHTS_0");
            let skin = mesh.skin().unwrap();
            assert_eq!(skin.len(), mesh.vertices().len());

            for vertex in skin {
                // 权重必须归一化，否则顶点会被整体拉离骨骼。
                assert!(
                    (vertex.weight_sum() - 1.0).abs() < 1e-3,
                    "权重和为 {}",
                    vertex.weight_sum()
                );
            }
        }
    }

    #[test]
    fn soldier_skinned_nodes_reference_their_skin() {
        let Some(model) = soldier() else { return };
        let model = model.data_ref().unwrap();

        let skinned: Vec<&ModelNode> = model
            .nodes()
            .iter()
            .filter(|node| node.skin.is_some())
            .collect();

        assert_eq!(skinned.len(), 2);
        for node in skinned {
            let skin = node.skin.unwrap();
            assert!(skin < model.skins().len());
            // 挂着骨架的节点自己得有几何体，否则蒙皮无从谈起。
            assert!(!node.parts.is_empty());
        }
    }

    #[test]
    fn animation_targets_cover_the_skeleton() {
        let Some(model) = soldier() else { return };
        let model = model.data_ref().unwrap();

        let skin = &model.skins()[0];
        let idle = &model.animations()[0];
        let targets: std::collections::HashSet<usize> =
            idle.tracks().iter().map(|track| track.target).collect();

        // 动画驱动的节点应当覆盖骨架的全部关节，
        // 少一个就意味着那根骨头永远停在绑定姿态。
        let missing: Vec<usize> = skin
            .joints
            .iter()
            .copied()
            .filter(|joint| !targets.contains(joint))
            .collect();
        assert!(missing.is_empty(), "这些关节没有动画轨道：{missing:?}");
    }

    #[test]
    fn static_model_has_no_skins_or_animations() {
        // 静态模型不该凭空多出骨架，也不该被当成动画模型。
        let model = load(&triangle_gltf(false), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();

        assert!(model.skins().is_empty());
        assert!(model.animations().is_empty());
        assert!(!model.is_animated());
        assert!(!model.meshes()[0].is_skinned());
        assert_eq!(model.node(0).unwrap().skin, None);
    }

    /// 加载仓库里的 lion.glb —— 一个带形变目标的模型
    /// （4 个网格各有一个形变：mouth / leftEye / rightEye / tongue）。
    fn lion() -> Option<Resource<Model>> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/lion.glb");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("跳过依赖 assets/lion.glb 的测试：文件不存在");
            return None;
        };

        let mut io = MemoryResourceIo::new();
        io.add("lion.glb", bytes);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(GltfLoader);
        Some(
            manager
                .request_blocking::<Model>("lion.glb")
                .expect("lion.glb 应当能加载"),
        )
    }

    #[test]
    fn lion_imports_its_morph_targets() {
        let Some(model) = lion() else { return };
        let model = model.data_ref().unwrap();

        let morphed: Vec<&kmesh::Mesh> = model
            .meshes()
            .iter()
            .filter(|mesh| mesh.has_morph_targets())
            .collect();

        // 五个网格里有四个带形变，每个各一个目标。
        assert_eq!(morphed.len(), 4);
        for mesh in &morphed {
            assert_eq!(mesh.morph_target_count(), 1);
            // 增量必须与顶点一一对应，否则网格会被撕开。
            assert_eq!(mesh.morph_targets()[0].len(), mesh.vertices().len());
        }
    }

    #[test]
    fn lion_reads_target_names_from_extras() {
        let Some(model) = lion() else { return };
        let model = model.data_ref().unwrap();

        let mut names: Vec<&str> = model
            .meshes()
            .iter()
            .filter(|mesh| mesh.has_morph_targets())
            .map(|mesh| mesh.morph_targets()[0].name())
            .collect();
        names.sort_unstable();

        // 名字藏在 mesh.extras 的 targetNames 里，规范没给它专门的字段。
        assert_eq!(names, vec!["leftEye", "mouth", "rightEye", "tongue"]);
    }

    #[test]
    fn lion_keeps_the_default_weights() {
        let Some(model) = lion() else { return };
        let model = model.data_ref().unwrap();

        // mouth 与 tongue 的默认权重是 1，两只眼睛是 0。
        let weights: Vec<(&str, f32)> = model
            .meshes()
            .iter()
            .filter(|mesh| mesh.has_morph_targets())
            .map(|mesh| (mesh.morph_targets()[0].name(), mesh.morph_weights()[0]))
            .collect();

        for (name, weight) in weights {
            let expected = match name {
                "mouth" | "tongue" => 1.0,
                _ => 0.0,
            };
            assert_eq!(weight, expected, "{name} 的默认权重不对");
        }
    }

    #[test]
    fn lion_morph_deltas_actually_move_vertices() {
        let Some(model) = lion() else { return };
        let model = model.data_ref().unwrap();

        let mesh = model
            .meshes()
            .iter()
            .find(|mesh| mesh.find_morph_target("mouth").is_some())
            .expect("应当有 mouth 形变");

        // 权重为 0 时形变不改变任何顶点；权重为 1 时至少有一个顶点动了。
        let moved = (0..mesh.vertices().len()).any(|vertex| {
            let rest = mesh.morphed_position(vertex, &[0.0]);
            let full = mesh.morphed_position(vertex, &[1.0]);
            (full - rest).length() > 1e-4
        });
        assert!(moved, "形变目标里全是零增量");

        // 静止状态必须与原始顶点一致。
        for vertex in 0..mesh.vertices().len() {
            assert_eq!(
                mesh.morphed_position(vertex, &[0.0]),
                mesh.vertices()[vertex].position()
            );
        }
    }

    #[test]
    fn morph_weight_channels_split_into_per_target_tracks() {
        // 合成一个 weights 动画：两个形变目标、三个关键帧，值是交错排列的。
        // lion.glb 本身没有动画，这条路径只能靠合成数据验证。
        let mut buffer = Vec::new();
        for time in [0.0f32, 0.5, 1.0] {
            buffer.extend_from_slice(&time.to_le_bytes());
        }
        // 每帧两个权重：(0,1) → (0.5,0.5) → (1,0)
        for weights in [[0.0f32, 1.0], [0.5, 0.5], [1.0, 0.0]] {
            for weight in weights {
                buffer.extend_from_slice(&weight.to_le_bytes());
            }
        }
        // 一个退化三角形的顶点与形变增量。
        for value in [0.0f32; 9] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for value in [1.0f32; 9] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for value in [2.0f32; 9] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }

        let source = r#"{
          "asset": {"version": "2.0"},
          "scene": 0,
          "scenes": [{"nodes": [0]}],
          "nodes": [{"mesh": 0}],
          "meshes": [{
            "weights": [0.0, 1.0],
            "extras": {"targetNames": ["open", "close"]},
            "primitives": [{
              "attributes": {"POSITION": 2},
              "targets": [{"POSITION": 3}, {"POSITION": 4}]
            }]
          }],
          "animations": [{
            "name": "Talk",
            "channels": [{"sampler": 0, "target": {"node": 0, "path": "weights"}}],
            "samplers": [{"input": 0, "output": 1, "interpolation": "LINEAR"}]
          }],
          "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "SCALAR"},
            {"bufferView": 1, "componentType": 5126, "count": 6, "type": "SCALAR"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0,0,0], "max": [0,0,0]},
            {"bufferView": 3, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [1,1,1], "max": [1,1,1]},
            {"bufferView": 4, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [2,2,2], "max": [2,2,2]}
          ],
          "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 12},
            {"buffer": 0, "byteOffset": 12, "byteLength": 24},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 36},
            {"buffer": 0, "byteOffset": 108, "byteLength": 36}
          ],
          "buffers": [{"byteLength": 144, "uri": "morph.bin"}]
        }"#;

        let mut io = MemoryResourceIo::new();
        io.add("morph.gltf", source);
        io.add("morph.bin", buffer);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(GltfLoader);
        let model = manager
            .request_blocking::<Model>("morph.gltf")
            .expect("形变动画应当能加载");
        let model = model.data_ref().unwrap();

        let clip = &model.animations()[0];
        // 一个 weights 通道驱动两个目标 → 拆成两条标量轨道。
        assert_eq!(clip.tracks().len(), 2);

        let pose = clip.sample(0.5);
        assert_eq!(pose.morph(0, 0), Some(0.5));
        assert_eq!(pose.morph(0, 1), Some(0.5));

        // 两端的值也要对得上，说明交错的采样值拆对了位置。
        let start = clip.sample(0.0);
        assert_eq!(start.morph(0, 0), Some(0.0));
        assert_eq!(start.morph(0, 1), Some(1.0));

        // 默认权重与目标名一并读进来。
        let mesh = &model.meshes()[0];
        assert_eq!(mesh.morph_weights(), &[0.0, 1.0]);
        assert_eq!(mesh.find_morph_target("close"), Some(1));
    }

    #[test]
    fn model_without_morph_targets_stays_clean() {
        let model = load(&triangle_gltf(false), "tri.gltf").unwrap();
        let model = model.data_ref().unwrap();

        assert!(!model.meshes()[0].has_morph_targets());
        assert!(model.meshes()[0].morph_weights().is_empty());
    }
}
