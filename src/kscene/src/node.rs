//! 场景节点。

use crate::audio::SoundSource;
use crate::physics::{Collider, Joint, RigidBody};
use crate::ragdoll::Ragdoll;
use crate::script::ScriptSlot;
use crate::skin::{AnimationPlayer, Skin};
use crate::{Camera, Mesh, Transform};
use kcore::pool::Handle;
use klight::Light;
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
    /// 是否能被贴花贴上。
    ///
    /// 贴花节点自己会被设成 `false`：不然第二发子弹打在同一处时，
    /// 会把第一个贴花当接收面贴上去，一层层叠起来，每层都比上一层
    /// 高一个 `offset`，很快就浮在半空了。水面、植被这类东西
    /// 通常也该关掉。
    pub receives_decals: bool,
    /// 这个物体接受**哪些层**的光照。位掩码，和光源的
    /// [`Light::mask`](klight::Light::mask) 按位与，非零才被照亮。
    ///
    /// 默认全 1（接受一切）。两边都得同意：任一方把对方的层关掉就不照。
    ///
    /// 典型用法是「这盏灯只打在角色身上」——把角色放进一个单独的层，
    /// 那盏灯只开那一层。拿「把灯挪远」之类的物理手段去凑永远凑不准。
    pub light_mask: u32,

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
    /// 刚体。装箱的理由与粒子一样：绝大多数节点没有物理组件，
    /// 内联进来等于让每个 `Node` 白背上百字节。
    pub(crate) rigid_body: Option<Box<RigidBody>>,
    pub(crate) collider: Option<Box<Collider>>,
    pub(crate) joint: Option<Box<Joint>>,
    pub(crate) ragdoll: Option<Box<Ragdoll>>,
    /// 地形。块网格由 [`Scene::update`] 生成成子节点。
    pub(crate) terrain: Option<Box<kterrain::Terrain>>,
    pub(crate) sound: Option<Box<SoundSource>>,
    pub(crate) script: Option<Box<ScriptSlot>>,
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
            receives_decals: true,
            light_mask: u32::MAX,
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
            rigid_body: None,
            collider: None,
            joint: None,
            ragdoll: None,
            terrain: None,
            sound: None,
            script: None,
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

    /// 指定这个物体接受哪些层的光照。见 [`light_mask`](Self::light_mask)。
    pub fn with_light_mask(mut self, mask: u32) -> Self {
        self.light_mask = mask;
        self
    }

    /// 挂上相机。
    pub fn with_camera(mut self, camera: Camera) -> Self {
        self.camera = Some(camera);
        self
    }

    /// 就地挂上（或换掉）光源。
    ///
    /// 和 [`with_light`](Self::with_light) 的区别只在借用形式，但差别很实在：
    /// 那个吃掉 `self`，只能在构造节点时用。运行时给一个已经在场景里的节点
    /// 挂上灯，就得走这条。
    ///
    /// [`light_mut`](Self::light_mut) 也不顶用——组件还不存在时它返回
    /// `None`，而「本来没有、现在要有」正是这里要解决的事。
    pub fn set_light(&mut self, light: Light) {
        self.light = Some(light);
    }

    /// 就地挂上（或换掉）相机。
    ///
    /// 运行时在透视与正交之间切换走这条：换的是整个 [`Camera`]，
    /// 而不是去改它的某个字段。
    pub fn set_camera(&mut self, camera: Camera) {
        self.camera = Some(camera);
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

    /// 挂上刚体。
    ///
    /// 刚体的初始位姿取自本节点的世界变换。之后谁驱动谁取决于刚体类型：
    /// 动态刚体由物理驱动节点，静态与运动学刚体反过来。
    pub fn with_rigid_body(mut self, body: RigidBody) -> Self {
        self.rigid_body = Some(Box::new(body));
        self
    }

    /// 挂上碰撞体。
    ///
    /// 绑定到本节点或最近的带刚体的祖先节点；一个都没有就是静态几何。
    pub fn with_collider(mut self, collider: Collider) -> Self {
        self.collider = Some(Box::new(collider));
        self
    }

    /// 挂上关节。
    pub fn with_joint(mut self, joint: Joint) -> Self {
        self.joint = Some(Box::new(joint));
        self
    }

    /// 挂上声源。声音的位置跟着本节点的世界变换走。
    pub fn with_sound(mut self, sound: SoundSource) -> Self {
        self.sound = Some(Box::new(sound));
        self
    }

    /// 把已挂上的声源改成 3D 的。没有声源时什么都不做。
    pub fn with_sound_spatial(mut self, spatial: kaudio::Spatial) -> Self {
        if let Some(sound) = self.sound.as_deref_mut() {
            sound.spatial = Some(spatial);
        }
        self
    }

    /// 挂上脚本，参数是脚本资源路径。
    ///
    /// 生命周期由 `kscript` 驱动：`_ready` / `_process(delta)` / `_physics_process(delta)`。
    pub fn with_script(mut self, path: impl Into<String>) -> Self {
        self.script = Some(Box::new(ScriptSlot::new(path)));
        self
    }

    /// 挂上布娃娃。通常由
    /// [`RagdollBuilder`](crate::RagdollBuilder) 代劳，不必手写。
    /// 挂一块地形。
    pub fn with_terrain(mut self, terrain: kterrain::Terrain) -> Self {
        self.terrain = Some(Box::new(terrain));
        self
    }

    /// 挂一个布娃娃。
    pub fn with_ragdoll(mut self, ragdoll: Ragdoll) -> Self {
        self.ragdoll = Some(Box::new(ragdoll));
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

    /// 网格的可写引用。顶点动画、运行时改形状走它。
    ///
    /// 改完顶点位置记得 [`Mesh::recompute_bounds`]——包围盒不会自己跟着变，
    /// 而剔除拿它当真相：不重算的话，物体会在还该看得见的时候被剔掉，
    /// 或者反过来，包围盒大得离谱拖慢剔除。
    pub fn mesh_mut(&mut self) -> Option<&mut Mesh> {
        self.mesh.as_mut()
    }

    /// 材质的只读引用。
    pub fn material(&self) -> Option<&Material> {
        self.material.as_ref()
    }

    /// 挂上（或替换）网格。带形变目标时权重重置为网格自带的默认值。
    ///
    /// 与 [`with_mesh`](Self::with_mesh) 的区别只是它作用在已有的节点上。
    pub fn set_mesh(&mut self, mesh: Mesh) {
        self.morph_weights = mesh.morph_weights().to_vec();
        self.mesh = Some(mesh);
    }

    /// 挂上（或替换）材质。
    pub fn set_material(&mut self, material: Material) {
        self.material = Some(material);
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
        let Some(index) = self
            .mesh
            .as_ref()
            .and_then(|mesh| mesh.find_morph_target(name))
        else {
            return false;
        };
        self.set_morph_weight(index, weight);
        true
    }

    /// 按名字找形变目标的序号。
    pub fn find_morph_target(&self, name: &str) -> Option<usize> {
        self.mesh.as_ref()?.find_morph_target(name)
    }

    /// 刚体的只读引用。
    pub fn rigid_body(&self) -> Option<&RigidBody> {
        self.rigid_body.as_deref()
    }

    /// 刚体的可变引用，用来施加冲量、切换类型、瞬移。
    pub fn rigid_body_mut(&mut self) -> Option<&mut RigidBody> {
        self.rigid_body.as_deref_mut()
    }

    /// 碰撞体的只读引用。
    /// 挂一个碰撞体。
    pub fn set_collider(&mut self, collider: Collider) {
        self.collider = Some(Box::new(collider));
    }

    /// 碰撞体。
    pub fn collider(&self) -> Option<&Collider> {
        self.collider.as_deref()
    }

    /// 碰撞体的可变引用。
    pub fn collider_mut(&mut self) -> Option<&mut Collider> {
        self.collider.as_deref_mut()
    }

    /// 关节的只读引用。
    pub fn joint(&self) -> Option<&Joint> {
        self.joint.as_deref()
    }

    /// 关节的可变引用。
    pub fn joint_mut(&mut self) -> Option<&mut Joint> {
        self.joint.as_deref_mut()
    }

    /// 布娃娃的只读引用。
    pub fn ragdoll(&self) -> Option<&Ragdoll> {
        self.ragdoll.as_deref()
    }

    /// 布娃娃的可变引用，用来开关它。
    pub fn ragdoll_mut(&mut self) -> Option<&mut Ragdoll> {
        self.ragdoll.as_deref_mut()
    }

    /// 挂上布娃娃。
    pub fn set_ragdoll(&mut self, ragdoll: Ragdoll) {
        self.ragdoll = Some(Box::new(ragdoll));
    }

    /// 挂一块地形。
    pub fn set_terrain(&mut self, terrain: kterrain::Terrain) {
        self.terrain = Some(Box::new(terrain));
    }

    /// 地形。
    pub fn terrain(&self) -> Option<&kterrain::Terrain> {
        self.terrain.as_deref()
    }

    /// 地形的可变引用。
    pub fn terrain_mut(&mut self) -> Option<&mut kterrain::Terrain> {
        self.terrain.as_deref_mut()
    }

    /// 把地形整个取走，节点上留空。
    ///
    /// 引擎内部用：更新 LOD 时既要读地形又要往场景里加子节点，
    /// 两个可变借用碰在一起。取出来用完再 [`set_terrain`](Self::set_terrain) 放回去。
    pub(crate) fn take_terrain(&mut self) -> Option<kterrain::Terrain> {
        self.terrain.take().map(|t| *t)
    }

    /// 声源的只读引用。
    pub fn sound(&self) -> Option<&SoundSource> {
        self.sound.as_deref()
    }

    /// 声源的可变引用，用来改音量、暂停、重放。
    pub fn sound_mut(&mut self) -> Option<&mut SoundSource> {
        self.sound.as_deref_mut()
    }

    /// 挂上（或替换）声源。
    pub fn set_sound(&mut self, sound: SoundSource) {
        self.sound = Some(Box::new(sound));
    }

    /// 脚本槽位的只读引用。
    pub fn script(&self) -> Option<&ScriptSlot> {
        self.script.as_deref()
    }

    /// 脚本槽位的可变引用，用来开关它或换一份脚本。
    pub fn script_mut(&mut self) -> Option<&mut ScriptSlot> {
        self.script.as_deref_mut()
    }

    /// 挂上（或替换）脚本槽位。
    pub fn set_script(&mut self, slot: ScriptSlot) {
        self.script = Some(Box::new(slot));
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
