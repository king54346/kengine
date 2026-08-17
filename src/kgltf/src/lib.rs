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

pub use loader::GltfLoader;
pub use model::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, NodeTransform};

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
}
