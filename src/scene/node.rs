//! 场景节点。

use crate::scene::{Camera, Mesh, Transform};
use kcore::pool::Handle;
use kmaterial::Material;
use kmath::{Aabb, Mat4, Vec3};

/// 场景树中的一个节点。
///
/// 节点自身只持有局部变换；世界变换由 [`Scene::update`](crate::scene::Scene::update)
/// 在每帧沿树自上而下算出。
#[derive(Debug)]
pub struct Node {
    /// 节点名称，用于查找与调试。
    pub name: String,
    /// 相对父节点的变换。
    pub transform: Transform,
    /// 是否参与渲染。设为 `false` 时该节点及其子树都不会被绘制。
    pub visible: bool,

    pub(crate) mesh: Option<Mesh>,
    pub(crate) material: Option<Material>,
    pub(crate) camera: Option<Camera>,
    pub(crate) parent: Handle<Node>,
    pub(crate) children: Vec<Handle<Node>>,
    pub(crate) global_transform: Mat4,
    /// 自身可见且所有祖先都可见。
    pub(crate) global_visible: bool,
    /// 世界空间包围盒，由 `Scene::update` 维护。
    pub(crate) global_aabb: Aabb,
}

impl Default for Node {
    fn default() -> Self {
        Self::new("Node")
    }
}

impl Node {
    /// 创建一个只有名字的空节点。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transform: Transform::IDENTITY,
            visible: true,
            mesh: None,
            material: None,
            camera: None,
            parent: Handle::NONE,
            children: Vec::new(),
            global_transform: Mat4::IDENTITY,
            global_visible: true,
            global_aabb: Aabb::EMPTY,
        }
    }

    /// 挂上网格，使该节点可被绘制。
    pub fn with_mesh(mut self, mesh: Mesh) -> Self {
        self.mesh = Some(mesh);
        self
    }

    /// 指定材质。未指定时渲染器使用标准材质。
    pub fn with_material(mut self, material: Material) -> Self {
        self.material = Some(material);
        self
    }

    /// 挂上相机。
    pub fn with_camera(mut self, camera: Camera) -> Self {
        self.camera = Some(camera);
        self
    }

    /// 指定局部变换。
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }

    /// 指定局部位置。
    pub fn with_position(mut self, position: Vec3) -> Self {
        self.transform.position = position;
        self
    }

    /// 指定局部缩放。
    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.transform.scale = scale;
        self
    }

    /// 网格的只读引用。
    pub fn mesh(&self) -> Option<&Mesh> {
        self.mesh.as_ref()
    }

    /// 材质的只读引用。
    pub fn material(&self) -> Option<&Material> {
        self.material.as_ref()
    }

    /// 材质的可变引用，可在运行时改颜色、换贴图。
    pub fn material_mut(&mut self) -> Option<&mut Material> {
        self.material.as_mut()
    }

    /// 相机的只读引用。
    pub fn camera(&self) -> Option<&Camera> {
        self.camera.as_ref()
    }

    /// 相机的可变引用。
    pub fn camera_mut(&mut self) -> Option<&mut Camera> {
        self.camera.as_mut()
    }

    /// 父节点句柄；根节点返回 [`Handle::NONE`]。
    pub fn parent(&self) -> Handle<Node> {
        self.parent
    }

    /// 子节点句柄列表。
    pub fn children(&self) -> &[Handle<Node>] {
        &self.children
    }

    /// 上一次 [`Scene::update`](crate::scene::Scene::update) 算出的世界变换矩阵。
    pub fn global_transform(&self) -> Mat4 {
        self.global_transform
    }

    /// 世界空间的轴对齐包围盒，上一次 `Scene::update` 算出。
    ///
    /// 没有网格的节点返回空包围盒。
    pub fn global_aabb(&self) -> Aabb {
        self.global_aabb
    }

    /// 世界空间中的位置。
    pub fn global_position(&self) -> Vec3 {
        self.global_transform.to_scale_rotation_translation().2
    }
}
