//! 导入结果的数据结构。
//!
//! 这里刻意不依赖引擎的 `Scene`——kgltf 只产出中立的模型描述，
//! 由引擎负责把它实例化成场景节点。

use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3};
use kmesh::Mesh;

/// [`Model`] 的资源类型标识。
pub const MODEL_TYPE_UUID: Uuid = uuid!("a7d3f018-52b6-4e91-8c07-3f5a9e1d64b2");

/// 节点的局部变换。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeTransform {
    /// 位置。
    pub position: Vec3,
    /// 旋转。
    pub rotation: Quat,
    /// 缩放。
    pub scale: Vec3,
}

impl Default for NodeTransform {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl NodeTransform {
    /// 组合成局部变换矩阵。
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.position)
    }
}

/// 节点上的一块几何体：一个网格配一个材质。
///
/// glTF 的一个 mesh 可以有多个 primitive，每个 primitive 材质不同，
/// 因此这里拆成多块。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshPart {
    /// 索引到 [`Model::meshes`]。
    pub mesh: usize,
    /// 索引到 [`Model::materials`]；glTF 允许 primitive 不指定材质。
    pub material: Option<usize>,
}

/// 模型里的一个节点。
#[derive(Debug, Clone)]
pub struct ModelNode {
    /// 节点名，来自 glTF；没有名字时为空串。
    pub name: String,
    /// 局部变换。
    pub transform: NodeTransform,
    /// 子节点在 [`Model::nodes`] 中的索引。
    pub children: Vec<usize>,
    /// 该节点携带的几何体。
    pub parts: Vec<MeshPart>,
}

impl Default for ModelNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            transform: NodeTransform::default(),
            children: Vec::new(),
            parts: Vec::new(),
        }
    }
}

/// 一个导入完成的模型。
#[derive(Debug, Clone)]
pub struct Model {
    id: Uuid,
    pub(crate) meshes: Vec<Mesh>,
    pub(crate) materials: Vec<Material>,
    pub(crate) nodes: Vec<ModelNode>,
    pub(crate) roots: Vec<usize>,
}

impl Model {
    /// 直接用现成数据构造模型。
    ///
    /// glTF 导入走这条路径；从其他格式导入或程序化生成模型时同样可用。
    pub fn new(
        meshes: Vec<Mesh>,
        materials: Vec<Material>,
        nodes: Vec<ModelNode>,
        roots: Vec<usize>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            meshes,
            materials,
            nodes,
            roots,
        }
    }

    /// 资源标识。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 全部网格。
    pub fn meshes(&self) -> &[Mesh] {
        &self.meshes
    }

    /// 全部材质。
    pub fn materials(&self) -> &[Material] {
        &self.materials
    }

    /// 全部节点。
    pub fn nodes(&self) -> &[ModelNode] {
        &self.nodes
    }

    /// 根节点索引。
    pub fn roots(&self) -> &[usize] {
        &self.roots
    }

    /// 按索引取节点。
    pub fn node(&self, index: usize) -> Option<&ModelNode> {
        self.nodes.get(index)
    }

    /// 按索引取网格。
    pub fn mesh(&self, index: usize) -> Option<&Mesh> {
        self.meshes.get(index)
    }

    /// 按索引取材质。
    pub fn material(&self, index: usize) -> Option<&Material> {
        self.materials.get(index)
    }

    /// 模型的三角形总数。
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(Mesh::triangle_count).sum()
    }

    /// 按名称查找节点。
    pub fn find_node(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|node| node.name == name)
    }
}

impl ResourceData for Model {
    fn type_uuid(&self) -> Uuid {
        MODEL_TYPE_UUID
    }
}
