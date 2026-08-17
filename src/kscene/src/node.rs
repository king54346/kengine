//! 场景节点。

use crate::{Camera, Mesh, Transform};
use klight::Light;
use kcore::pool::Handle;
use kmaterial::Material;
use kmath::{Aabb, Mat4, Vec3};
use crate::skin::{AnimationPlayer, Skin};
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
    /// 骨架。有它的节点走蒙皮渲染路径。
    pub(crate) skin: Option<Box<Skin>>,
    /// 动画播放器，通常挂在模型的根节点上。
    pub(crate) animator: Option<Box<AnimationPlayer>>,
    /// 当前的形变权重，与网格的形变目标一一对应。
    ///
    /// 存在节点而不是网格上：网格是共享资源，同一张脸的两个实例
    /// 要能各做各的表情。
    pub(crate) morph_weights: Vec<f32>,
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
            skin: None,
            animator: None,
            morph_weights: Vec::new(),
            parent: Handle::NONE,
            children: Vec::new(),
            global_transform: Mat4::IDENTITY,
            global_visible: true,
            global_aabb: Aabb::EMPTY,
        }
    }

    /// 挂上网格，使该节点可被绘制。
    ///
    /// 网格带形变目标时，权重初始化为网格自带的默认值。
    pub fn with_mesh(mut self, mesh: Mesh) -> Self {
        self.morph_weights = mesh.morph_weights().to_vec();
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

    /// 挂上骨架，使该节点的网格随骨骼变形。
    pub fn with_skin(mut self, skin: Skin) -> Self {
        self.skin = Some(Box::new(skin));
        self
    }

    /// 挂上动画播放器。
    pub fn with_animator(mut self, animator: AnimationPlayer) -> Self {
        self.animator = Some(Box::new(animator));
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

    /// 骨架的只读引用。
    pub fn skin(&self) -> Option<&Skin> {
        self.skin.as_deref()
    }

    /// 骨架的可变引用。
    pub fn skin_mut(&mut self) -> Option<&mut Skin> {
        self.skin.as_deref_mut()
    }

    /// 动画播放器的只读引用。
    pub fn animator(&self) -> Option<&AnimationPlayer> {
        self.animator.as_deref()
    }

    /// 动画播放器的可变引用，用来切剪辑、调权重。
    pub fn animator_mut(&mut self) -> Option<&mut AnimationPlayer> {
        self.animator.as_deref_mut()
    }

    /// 当前的形变权重。
    pub fn morph_weights(&self) -> &[f32] {
        &self.morph_weights
    }

    /// 形变权重的可变引用。
    pub fn morph_weights_mut(&mut self) -> &mut [f32] {
        &mut self.morph_weights
    }

    /// 设置某个形变目标的权重。序号越界时忽略。
    ///
    /// 不夹到 `[0, 1]`：glTF 规范并不限制取值范围，
    /// 超出范围会得到夸张的外插效果，有时正是想要的。
    pub fn set_morph_weight(&mut self, index: usize, weight: f32) {
        if let Some(slot) = self.morph_weights.get_mut(index) {
            *slot = weight;
        }
    }

    /// 按形变目标的名字设置权重，返回是否找到了这个名字。
    pub fn set_morph_weight_by_name(&mut self, name: &str, weight: f32) -> bool {
        let Some(index) = self.mesh.as_ref().and_then(|mesh| mesh.find_morph_target(name)) else {
            return false;
        };
        self.set_morph_weight(index, weight);
        true
    }

    /// 按名字找形变目标的序号。
    pub fn find_morph_target(&self, name: &str) -> Option<usize> {
        self.mesh.as_ref()?.find_morph_target(name)
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
