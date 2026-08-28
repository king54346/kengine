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
#[derive(Debug, Clone, Default)]
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

/// glTF 文件里挂着的 `extras`，按原文保留。
///
/// `extras` 是 glTF 规范留给各家自己塞东西的口袋：Blender 的自定义属性、
/// 关卡编辑器导出的标记（「这是出生点」「这扇门锁着」）都走它。规范只说它是
/// 一段任意 JSON，**没有约定任何结构**，所以引擎能做的最多也就是原样留下来，
/// 由游戏自己解释。
///
/// 因此这里存的是**未解析的 JSON 文本**。引擎不替谁选 JSON 库，也不猜里面
/// 该有什么字段——那是游戏的事。
///
/// 各个 `Vec` 按对应资源的序号对齐（节点序号、材质序号……），没有 extras 的
/// 那一项是 [`None`]。
///
/// 这套东西**只住在 kgltf**：`extras` 是 glTF 特有的概念，把它塞进
/// [`Mesh`] 或 [`Material`] 会让那两个通用类型背上一个只有一种格式才有的字段。
/// （材质的**名字**是另一回事——任何格式的材质都可以有名字，所以那个字段
/// 在 `kmaterial` 里。）
#[derive(Debug, Clone, Default)]
pub struct GltfExtras {
    /// 场景级：`scenes[i].extras`。
    pub scene: Option<String>,
    /// 按节点序号：`nodes[i].extras`。
    pub nodes: Vec<Option<String>>,
    /// 按 glTF 的 mesh 序号：`meshes[i].extras`。
    ///
    /// 注意是 **glTF 的 mesh 序号**，不是 [`Model::meshes`] 的下标：
    /// 一个 glTF mesh 含多个 primitive 时会展开成好几张 [`Mesh`]。
    pub meshes: Vec<Option<String>>,
    /// 按材质序号：`materials[i].extras`。
    pub materials: Vec<Option<String>>,
}

impl GltfExtras {
    /// 一个节点的 extras。
    pub fn node(&self, index: usize) -> Option<&str> {
        self.nodes.get(index)?.as_deref()
    }

    /// 一个材质的 extras。
    pub fn material(&self, index: usize) -> Option<&str> {
        self.materials.get(index)?.as_deref()
    }

    /// 一个 glTF mesh 的 extras。
    pub fn mesh(&self, index: usize) -> Option<&str> {
        self.meshes.get(index)?.as_deref()
    }

    /// 这个文件里一条 extras 都没有。
    pub fn is_empty(&self) -> bool {
        self.scene.is_none()
            && self.nodes.iter().all(Option::is_none)
            && self.meshes.iter().all(Option::is_none)
            && self.materials.iter().all(Option::is_none)
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
    pub(crate) extras: GltfExtras,
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
            extras: GltfExtras::default(),
        }
    }

    /// 附上 glTF 的 `extras`。
    pub fn with_extras(mut self, extras: GltfExtras) -> Self {
        self.extras = extras;
        self
    }

    /// 文件里挂着的 `extras`，原文保留。
    ///
    /// 配合 [`Scene::instantiate_model_mapped`](../kscene/struct.Scene.html#method.instantiate_model_mapped)
    /// 就能把「第几个节点上写着什么标记」对应到实例出来的场景节点上。
    pub fn extras(&self) -> &GltfExtras {
        &self.extras
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
