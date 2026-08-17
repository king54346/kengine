//! 场景节点。

use crate::{Camera, Mesh, Transform};
use klight::Light;
use kcore::pool::Handle;
use kmaterial::Material;
use kmath::{Aabb, Mat4, Vec3};
use kparticle::ParticleSystem;

/// 场景树中的一个节点。
///
/// 节点自身只持有局部变换；世界变换由 [`Scene::update`](crate::Scene::update)
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
    pub(crate) light: Option<Light>,
    pub(crate) particles: Option<Box<ParticleSystem>>,
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
            light: None,
            // 装箱：粒子系统里有九个数组，直接内联会把每个 Node 撑大一大截，
            // 而绝大多数节点根本没有粒子。
            particles: None,
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

    /// 挂上光源。位置与朝向取自本节点的世界变换（照射方向为 -Z）。
    pub fn with_light(mut self, light: Light) -> Self {
        self.light = Some(light);
        self
    }

    /// 挂上相机。
    pub fn with_camera(mut self, camera: Camera) -> Self {
        self.camera = Some(camera);
        self
    }

    /// 挂上粒子系统。发射器的位置与朝向取自本节点的世界变换。
    pub fn with_particles(mut self, particles: ParticleSystem) -> Self {
        self.particles = Some(Box::new(particles));
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

    /// 光源的只读引用。
    pub fn light(&self) -> Option<&Light> {
        self.light.as_ref()
    }

    /// 光源的可变引用，可在运行时改颜色与强度。
    pub fn light_mut(&mut self) -> Option<&mut Light> {
        self.light.as_mut()
    }

    /// 粒子系统的只读引用。
    pub fn particles(&self) -> Option<&ParticleSystem> {
        self.particles.as_deref()
    }

    /// 粒子系统的可变引用，可在运行时改参数或手动喷发。
    pub fn particles_mut(&mut self) -> Option<&mut ParticleSystem> {
        self.particles.as_deref_mut()
    }

    /// 父节点句柄；根节点返回 [`Handle::NONE`]。
    pub fn parent(&self) -> Handle<Node> {
        self.parent
    }

    /// 子节点句柄列表。
    pub fn children(&self) -> &[Handle<Node>] {
        &self.children
    }

    /// 上一次 [`Scene::update`](crate::Scene::update) 算出的世界变换矩阵。
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
