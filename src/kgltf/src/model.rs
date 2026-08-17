//! 导入结果的数据结构。
//!
//! 这里刻意不依赖引擎的 `Scene`——kgltf 只产出中立的模型描述，
//! 由引擎负责把它实例化成场景节点。

use kanim::AnimationClip;
use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3};
use kmesh::Mesh;
use std::sync::Arc;

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

/// 一副骨架：一组关节，外加把顶点从模型空间搬进各关节局部空间的矩阵。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSkin {
    /// 关节对应的节点索引。蒙皮顶点里的关节号就是这个数组的下标。
    pub joints: Vec<usize>,
    /// 逆绑定矩阵，与 `joints` 一一对应。
    ///
    /// 它把顶点从模型空间变换到关节的局部空间；再乘上关节当前的世界变换，
    /// 顶点就跟着骨头动了。这是线性混合蒙皮的全部秘密。
    pub inverse_bind: Vec<Mat4>,
    /// 骨架根节点，glTF 允许不指定。
    pub skeleton: Option<usize>,
}

impl ModelSkin {
    /// 关节数量。
    pub fn len(&self) -> usize {
        self.joints.len()
    }

    /// 是否没有关节。
    pub fn is_empty(&self) -> bool {
        self.joints.is_empty()
    }
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
    /// 该节点使用的骨架，索引到 [`Model::skins`]。
    pub skin: Option<usize>,
}

impl Default for ModelNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            transform: NodeTransform::default(),
            children: Vec::new(),
            parts: Vec::new(),
            skin: None,
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
    pub(crate) skins: Vec<ModelSkin>,
    /// 动画剪辑。用 [`Arc`] 是为了让同一个模型的多个实例共享关键帧数据。
    pub(crate) animations: Arc<Vec<AnimationClip>>,
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
            skins: Vec::new(),
            animations: Arc::new(Vec::new()),
        }
    }

    /// 附上骨架。
    pub fn with_skins(mut self, skins: Vec<ModelSkin>) -> Self {
        self.skins = skins;
        self
    }

    /// 附上动画剪辑。
    pub fn with_animations(mut self, animations: Vec<AnimationClip>) -> Self {
        self.animations = Arc::new(animations);
        self
    }

    /// 全部骨架。
    pub fn skins(&self) -> &[ModelSkin] {
        &self.skins
    }

    /// 按索引取骨架。
    pub fn skin(&self, index: usize) -> Option<&ModelSkin> {
        self.skins.get(index)
    }

    /// 全部动画剪辑，可与其它实例共享。
    pub fn animations(&self) -> &Arc<Vec<AnimationClip>> {
        &self.animations
    }

    /// 按名字找动画剪辑。
    pub fn find_animation(&self, name: &str) -> Option<usize> {
        self.animations.iter().position(|clip| clip.name() == name)
    }

    /// 是否带骨骼动画。
    pub fn is_animated(&self) -> bool {
        !self.animations.is_empty() && !self.skins.is_empty()
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
