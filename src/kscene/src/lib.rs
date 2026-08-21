//! kscene —— 场景图。
//!
//! 一棵由 [`Node`] 组成的树，底层用 [`kcore::pool::Pool`] 存储。
//! 节点持有局部变换，世界变换与包围盒由 [`Scene::update`] 沿树自上而下算出。
//!
//! 本 crate **不依赖 wgpu**：渲染器通过 [`Scene::visible_meshes`] 拿到中立的
//! 绘制项列表，场景图本身不知道渲染后端的存在。

#![warn(missing_docs)]

mod audio;
pub mod decal;
mod cull;
mod debug;
mod node;
mod physics;
mod ragdoll;
mod script;
mod serialize;
mod skin;
mod streaming;
mod terrain;
mod transform;

pub use audio::SoundSource;
pub use debug::SceneDebugOptions;
pub use kaudio::{Attenuation, AudioBuffer, AudioDevice, Listener, Spatial};
pub use kcamera::{Camera, Frustum, Projection};
pub use kgizmo::{Color, Gizmos, Layer};
pub use kmath::{Aabb, Intersection};
pub use kmesh::{Mesh, Vertex};
pub use kparticle::ParticleSystem;
pub use kphysics::PhysicsDebugOptions;
pub use kphysics::{
    BodyHandle, ColliderDesc, ColliderHandle, ColliderShape, CollisionEvent, InteractionGroups,
    JointDesc, JointHandle, JointKind, PhysicsWorld, RayCastOptions, RayHit, RigidBodyDesc,
    RigidBodyType, ShapeCastOptions, SphericalLimits,
};
pub use ksprite::{SortMode, SpriteInstance, SpriteRegion};
pub use kterrain::{Brush, Terrain};
pub use node::Node;
pub use physics::{Collider, Joint, RigidBody};
pub use ragdoll::{LimbDesc, Ragdoll, RagdollBuilder, RagdollLimb, hinge_limits};
pub use script::ScriptSlot;
pub use serialize::SCENE_FORMAT_VERSION;
pub use skin::{AnimationPlayer, Skin};
pub use streaming::{Cell, CellState, Streaming, StreamingReport};
pub use transform::Transform;

use cull::SceneCulling;
use fxhash::FxHashMap;
use kanim::Animator;
use kcore::pool::{Handle, Pool};
use kgltf::Model;
use klight::Light;
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3};
use kpbr::Environment;
use std::ops::{Index, IndexMut};

/// 渲染器每帧收集到的一个绘制项。
///
/// 这是场景图与渲染器之间的唯一接口——渲染器不认识 [`Node`]，
/// 只消费这份中立的列表，因此两者不会互相纠缠。
#[derive(Debug)]
pub struct RenderItem<'a> {
    /// 待绘制的网格。
    pub mesh: &'a Mesh,
    /// 材质；为 [`None`] 时渲染器使用标准材质。
    pub material: Option<&'a Material>,
    /// 世界变换矩阵。
    ///
    /// 蒙皮网格这里是**单位阵**：按 glTF 规范，蒙皮网格自身节点的变换应当被忽略，
    /// 模型的位姿已经含在骨骼矩阵里了。着色器统一算 `model × skin × 顶点`，
    /// 静态网格的 skin 视为单位阵，两条路径因此共用一个公式。
    pub transform: Mat4,
    /// 世界空间包围盒，用于剔除。
    pub aabb: Aabb,
    /// 骨骼矩阵；静态网格为 [`None`]。
    pub skin: Option<&'a [Mat4]>,
    /// 形变权重，与网格的形变目标一一对应；没有形变时是空切片。
    pub morph_weights: &'a [f32],
}

/// 渲染器每帧收集到的一个粒子系统。
///
/// 与 [`RenderItem`] 一样是中立结构：渲染器不认识 [`Node`]，
/// 只拿到「一个粒子系统 + 它所在节点的世界变换」。
#[derive(Debug)]
pub struct ParticleItem<'a> {
    /// 粒子系统。
    pub system: &'a ParticleSystem,
    /// 所在节点的世界变换。局部空间的粒子要靠它变换到世界。
    pub transform: Mat4,
    /// 世界空间包围盒，用于剔除与排序。
    pub aabb: Aabb,
}

/// [`Scene::cast_ray`] 的结果，句柄已经解回场景节点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneRayHit {
    /// 被命中的碰撞体所在的节点。
    pub collider_node: Option<Handle<Node>>,
    /// 该碰撞体所属刚体所在的节点；独立碰撞体为 `None`。
    pub body_node: Option<Handle<Node>>,
    /// 世界空间命中点。
    pub point: Vec3,
    /// 命中处的表面法线。
    pub normal: Vec3,
    /// 起点到命中点的距离。
    pub distance: f32,
}

/// 绘制项该用的模型矩阵：蒙皮网格用单位阵，其余用节点的世界变换。
fn skinned_transform(node: &Node) -> Mat4 {
    if node.skin().is_some() {
        Mat4::IDENTITY
    } else {
        node.global_transform
    }
}

/// 一个场景。
///
/// 场景创建时自带一个名为 `__root` 的根节点，[`Scene::add_node`] 默认挂在它下面。
pub struct Scene {
    nodes: Pool<Node>,
    root: Handle<Node>,
    environment: Environment,
    /// 剔除加速结构，由 [`Scene::update`] 维护。
    culling: SceneCulling,
    /// 挂了特殊组件的节点句柄，由 [`Scene::update`] 顺带收集。
    index: NodeIndex,
    /// 物理世界。场景节点上的刚体 / 碰撞体 / 关节都在这里有一个对应物。
    physics: PhysicsWorld,
    /// 本帧的 2D 精灵。
    ///
    /// 和调试线同一个模式：即时模式，每帧重新提交，渲染器读走后清空。
    /// 走这条路的精灵享受专用的 2D 批处理；挂在节点上的 `Sprite`
    /// 走的仍然是 3D 管线（一个贴了图的方片）。
    sprites: Vec<ksprite::SpriteInstance>,
    /// 预滤波的 HDR 环境图 mip 链。渲染器靠它做镜面 IBL。
    prefiltered_environment: Option<std::sync::Arc<Vec<kpbr::prefilter::PrefilteredLevel>>>,
    /// 环境图的版本号。渲染器靠它判断要不要重传。
    ///
    /// 一条 256×128 的 mip 链是几兆的浮点数据，每帧重传纯属浪费；
    /// 而它只在换环境图时才变。
    environment_version: u64,
    /// 2D 精灵用到的贴图。
    ///
    /// 精灵实例只带一个纹理 id，渲染器得先见过贴图本身才画得出来。
    /// 存在场景上而不是让调用方直接找渲染器——插件拿得到场景，
    /// 拿不到渲染器。
    sprite_textures: Vec<ktexture::Texture>,
    /// 本帧的调试线。
    ///
    /// 放在场景上而不是渲染器上，是因为想画调试线的代码（游戏逻辑、
    /// 物理、脚本）拿得到的是场景，拿不到渲染器。渲染器每帧读走后清空。
    gizmos: Gizmos,
}

/// 按组件分类的节点句柄索引。
///
/// 光源、相机、粒子这些组件通常只挂在个位数的节点上，但为了找到它们
/// 而每帧把整个节点池扫一遍，代价是把几兆字节的节点数据灌进缓存——
/// 万级场景下这不只是慢，它还会把紧随其后的剔除所需的数据挤出缓存。
///
/// [`Scene::update`] 本来就要沿树走一遍，顺手记下句柄是免费的。
#[derive(Default)]
struct NodeIndex {
    /// 可见且带网格的节点，按树的深度优先顺序排列。
    drawables: Vec<Handle<Node>>,
    lights: Vec<Handle<Node>>,
    cameras: Vec<Handle<Node>>,
    particles: Vec<Handle<Node>>,
    skinned: Vec<Handle<Node>>,
    animators: Vec<Handle<Node>>,
    rigid_bodies: Vec<Handle<Node>>,
    colliders: Vec<Handle<Node>>,
    joints: Vec<Handle<Node>>,
    ragdolls: Vec<Handle<Node>>,
    sounds: Vec<Handle<Node>>,
    scripts: Vec<Handle<Node>>,
    terrains: Vec<Handle<Node>>,
}

impl NodeIndex {
    fn clear(&mut self) {
        self.drawables.clear();
        self.lights.clear();
        self.cameras.clear();
        self.particles.clear();
        self.skinned.clear();
        self.animators.clear();
        self.rigid_bodies.clear();
        self.colliders.clear();
        self.joints.clear();
        self.ragdolls.clear();
        self.sounds.clear();
        self.scripts.clear();
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    /// 创建一个只有根节点的空场景。
    pub fn new() -> Self {
        let mut nodes = Pool::new();
        let root = nodes.spawn(Node::new("__root"));
        Self {
            nodes,
            root,
            environment: Environment::default(),
            culling: SceneCulling::default(),
            index: NodeIndex::default(),
            physics: PhysicsWorld::new(),
            gizmos: Gizmos::new(),
            sprites: Vec::new(),
            sprite_textures: Vec::new(),
            prefiltered_environment: None,
            environment_version: 0,
        }
    }

    /// 用现成的节点池与根句柄组装一个场景。
    ///
    /// 给反序列化用：世界变换、包围盒、剔除结构、组件索引全是派生数据，
    /// 调用方拿到后应当立刻 [`update`](Self::update) 一次把它们算出来。
    pub(crate) fn from_parts(
        nodes: Pool<Node>,
        root: Handle<Node>,
        environment: Environment,
    ) -> Self {
        Self {
            nodes,
            root,
            environment,
            culling: SceneCulling::default(),
            index: NodeIndex::default(),
            physics: PhysicsWorld::new(),
            gizmos: Gizmos::new(),
            sprites: Vec::new(),
            sprite_textures: Vec::new(),
            prefiltered_environment: None,
            environment_version: 0,
        }
    }

    /// 根节点句柄。
    pub fn root(&self) -> Handle<Node> {
        self.root
    }

    /// 环境光设置。
    ///
    /// 方向光、点光源、聚光灯都是场景节点（见 [`Node::with_light`]），
    /// 这里只有代替 IBL 的常量环境光。
    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    /// 环境光设置的可变引用。
    pub fn environment_mut(&mut self) -> &mut Environment {
        &mut self.environment
    }

    /// 底层节点池的只读引用。
    pub fn nodes(&self) -> &Pool<Node> {
        &self.nodes
    }

    /// 把节点加入场景，挂在根节点下。
    pub fn add_node(&mut self, node: Node) -> Handle<Node> {
        self.add_node_with_parent(node, self.root)
    }

    /// 把节点加入场景并挂在指定父节点下。
    ///
    /// 若 `parent` 无效则回退到根节点。
    pub fn add_node_with_parent(&mut self, node: Node, parent: Handle<Node>) -> Handle<Node> {
        let parent = if self.nodes.is_valid_handle(parent) {
            parent
        } else {
            self.root
        };

        let handle = self.nodes.spawn(node);
        self.nodes[handle].parent = parent;
        self.nodes[parent].children.push(handle);

        // 物理索引在这里就地补上，而不是等下一次 `update`。
        // `step_physics` 排在 `update` 之前，只靠 `update` 收集的话，
        // 这一帧新加的刚体要到下一帧才进得了物理世界。
        self.index_physics_components(handle);

        handle
    }

    /// 把一个节点的物理组件登记进索引。已在索引里的不会重复登记。
    fn index_physics_components(&mut self, handle: Handle<Node>) {
        let Some(node) = self.nodes.try_borrow(handle).ok() else {
            return;
        };
        let (body, collider, joint, ragdoll) = (
            node.rigid_body.is_some(),
            node.collider.is_some(),
            node.joint.is_some(),
            node.ragdoll.is_some(),
        );
        if body && !self.index.rigid_bodies.contains(&handle) {
            self.index.rigid_bodies.push(handle);
        }
        if collider && !self.index.colliders.contains(&handle) {
            self.index.colliders.push(handle);
        }
        if joint && !self.index.joints.contains(&handle) {
            self.index.joints.push(handle);
        }
        if ragdoll && !self.index.ragdolls.contains(&handle) {
            self.index.ragdolls.push(handle);
        }
    }

    /// 把 `child` 改挂到 `parent` 下。
    pub fn link_nodes(&mut self, child: Handle<Node>, parent: Handle<Node>) {
        if child == self.root || !self.nodes.is_valid_handle(child) {
            return;
        }
        if !self.nodes.is_valid_handle(parent) || self.is_ancestor_of(child, parent) {
            // 不能把节点挂到自己的子孙下面，那会形成环。
            return;
        }

        self.detach_from_parent(child);
        self.nodes[child].parent = parent;
        self.nodes[parent].children.push(child);
    }

    /// 删除节点及其整棵子树。根节点会被忽略。
    pub fn remove_node(&mut self, handle: Handle<Node>) {
        if handle == self.root || !self.nodes.is_valid_handle(handle) {
            return;
        }

        self.detach_from_parent(handle);

        // 自下而上收集整棵子树，再统一释放。
        let mut to_free = Vec::new();
        let mut stack = vec![handle];
        while let Some(current) = stack.pop() {
            to_free.push(current);
            stack.extend_from_slice(&self.nodes[current].children);
        }
        for h in to_free {
            self.despawn_physics_of(h);
            self.nodes.free(h);
        }
    }

    /// 把一个节点在物理世界里的对应物删掉。
    ///
    /// 必须在释放节点**之前**做：句柄一旦回收就再也读不到组件里存的原生句柄，
    /// 物理世界里会留下一个谁都碰不到、却仍在参与模拟的幽灵。
    fn despawn_physics_of(&mut self, handle: Handle<Node>) {
        let Ok(node) = self.nodes.try_borrow_mut(handle) else {
            return;
        };
        let joint = node.joint.as_mut().and_then(|j| j.native());
        let collider = node.collider.as_mut().and_then(|c| c.native());
        let body = node.rigid_body.as_mut().and_then(|b| b.native());

        // 顺序要紧：关节 → 碰撞体 → 刚体。删刚体会连带删掉挂在它上面的
        // 碰撞体与关节，反过来先删刚体的话，后面两步就是在用失效句柄操作。
        if let Some(joint) = joint {
            self.physics.remove_joint(joint);
        }
        if let Some(collider) = collider {
            self.physics.remove_collider(collider);
        }
        if let Some(body) = body {
            self.physics.remove_body(body);
        }
    }

    /// 把导入的 glTF 模型实例化成场景节点，返回新建子树的根。
    ///
    /// 模型有多个根节点时，会额外建一个容器节点把它们收拢，
    /// 这样调用方拿到的永远是单个句柄。
    pub fn instantiate_model(&mut self, model: &Model, parent: Handle<Node>) -> Handle<Node> {
        let roots = model.roots();

        // 模型里的节点序号到场景句柄的映射。骨架的关节、动画的目标都是按序号引用的，
        // 建好这张表才能把它们接到具体的实例上。没被实例化的节点留 `Handle::NONE`。
        let mut mapping = vec![Handle::NONE; model.nodes().len()];

        // 单根模型直接实例化，不额外套一层。
        let root = if let [only] = roots {
            self.instantiate_model_node(model, *only, parent, &mut mapping)
        } else {
            let container = self.add_node_with_parent(Node::new("Model"), parent);
            for &node in roots {
                self.instantiate_model_node(model, node, container, &mut mapping);
            }
            container
        };

        self.attach_skins(model, &mapping);
        self.attach_animator(model, root, mapping);
        root
    }

    /// 给实例化出来的蒙皮网格节点挂上骨架。
    fn attach_skins(&mut self, model: &Model, mapping: &[Handle<Node>]) {
        for (index, source) in model.nodes().iter().enumerate() {
            let Some(skin_index) = source.skin else {
                continue;
            };
            let Some(skin) = model.skin(skin_index) else {
                continue;
            };

            let joints: Vec<Handle<Node>> = skin
                .joints
                .iter()
                .map(|&joint| mapping.get(joint).copied().unwrap_or(Handle::NONE))
                .collect();
            let skin = Skin::new(joints, skin.inverse_bind.clone());

            // 几何体被拆成多个子节点时（多材质），每一块都要各自的骨架。
            let handle = mapping.get(index).copied().unwrap_or(Handle::NONE);
            let children: Vec<Handle<Node>> = self
                .try_get(handle)
                .map(|node| node.children.clone())
                .unwrap_or_default();

            if let Ok(node) = self.nodes.try_borrow_mut(handle)
                && node.mesh.is_some()
            {
                node.skin = Some(Box::new(skin.clone()));
                continue;
            }
            for child in children {
                if let Ok(node) = self.nodes.try_borrow_mut(child)
                    && node.mesh.is_some()
                {
                    node.skin = Some(Box::new(skin.clone()));
                }
            }
        }
    }

    /// 给模型根节点挂上动画播放器。
    fn attach_animator(&mut self, model: &Model, root: Handle<Node>, mapping: Vec<Handle<Node>>) {
        if model.animations().is_empty() {
            return;
        }

        // 剪辑数据由 Arc 共享：同一个模型实例化多份时，各自只多出播放进度。
        let animator = Animator::new(model.animations().clone());
        if let Ok(node) = self.nodes.try_borrow_mut(root) {
            node.animator = Some(Box::new(AnimationPlayer::new(animator, mapping)));
        }
    }

    fn instantiate_model_node(
        &mut self,
        model: &Model,
        index: usize,
        parent: Handle<Node>,
        mapping: &mut [Handle<Node>],
    ) -> Handle<Node> {
        let Some(source) = model.node(index) else {
            return Handle::NONE;
        };

        let name = if source.name.is_empty() {
            format!("Node{index}")
        } else {
            source.name.clone()
        };

        let transform = Transform {
            position: source.transform.position,
            rotation: source.transform.rotation,
            scale: source.transform.scale,
        };
        let mut node = Node::new(name).with_transform(transform);

        // 单块几何体直接挂在本节点上；多块（多材质）则各建一个子节点，
        // 因为一个 Node 只能持有一份网格与材质。
        let single_part = source.parts.len() == 1;
        if single_part {
            let part = source.parts[0];
            if let Some(mesh) = model.mesh(part.mesh) {
                node = node.with_mesh(mesh.clone());
            }
            if let Some(material) = part.material.and_then(|i| model.material(i)) {
                node = node.with_material(material.clone());
            }
        }

        let handle = self.add_node_with_parent(node, parent);
        if let Some(slot) = mapping.get_mut(index) {
            *slot = handle;
        }

        if !single_part {
            for (part_index, part) in source.parts.iter().enumerate() {
                let Some(mesh) = model.mesh(part.mesh) else {
                    continue;
                };
                let mut child = Node::new(format!("Part{part_index}")).with_mesh(mesh.clone());
                if let Some(material) = part.material.and_then(|i| model.material(i)) {
                    child = child.with_material(material.clone());
                }
                self.add_node_with_parent(child, handle);
            }
        }

        for &child in &source.children.clone() {
            self.instantiate_model_node(model, child, handle, mapping);
        }

        handle
    }

    /// 按名称查找第一个匹配的节点。
    pub fn find_by_name(&self, name: &str) -> Option<Handle<Node>> {
        self.nodes
            .pair_iter()
            .find(|(_, node)| node.name == name)
            .map(|(handle, _)| handle)
    }

    /// 节点的只读引用，句柄无效时返回 [`None`]。
    pub fn try_get(&self, handle: Handle<Node>) -> Option<&Node> {
        self.nodes.try_borrow(handle).ok()
    }

    /// 节点的可变引用，句柄无效时返回 [`None`]。
    pub fn try_get_mut(&mut self, handle: Handle<Node>) -> Option<&mut Node> {
        self.nodes.try_borrow_mut(handle).ok()
    }

    /// 把另一个场景的全部节点并进本场景，挂在 `parent` 下，返回并入子树的根。
    ///
    /// 这是流式加载与预制体（prefab）共同的底层原语：一个「区块」或「预制体」
    /// 就是一个存盘的场景，用的时候整个并进来。
    ///
    /// # 句柄要整体重映射
    ///
    /// 两个场景各有各的节点池，来的那份里的句柄在本场景里指向的是别的东西
    /// （或者根本无效）。所以**每一处存着节点句柄的地方**都必须跟着改：
    /// 父子关系、骨架的关节表、关节组件两端的刚体。漏掉任何一处，
    /// 症状都是「并进来之后有几个节点莫名其妙地跟着另一个物体动」，
    /// 而且不会报错。
    ///
    /// 来源场景的环境设置（天空、环境光）**不并入**——那是全局的，
    /// 一个区块无权改写整张地图的天光。
    pub fn merge(&mut self, source: Scene, parent: Handle<Node>) -> Handle<Node> {
        let parent = if self.nodes.is_valid_handle(parent) {
            parent
        } else {
            self.root
        };

        let Scene {
            nodes: source_nodes,
            root: source_root,
            ..
        } = source;

        // ── 第一趟：把节点搬过来，记下「旧句柄 → 新句柄」 ──
        let mut remap: FxHashMap<Handle<Node>, Handle<Node>> = FxHashMap::default();
        let mut moved = Vec::new();
        for index in 0..source_nodes.get_capacity() {
            let old = source_nodes.handle_from_index(index);
            if !source_nodes.is_valid_handle(old) {
                continue;
            }
            moved.push(old);
        }

        let mut source_nodes = source_nodes;
        for old in &moved {
            let Ok(node) = source_nodes.try_borrow_mut(*old) else {
                continue;
            };
            // 用 `std::mem::take` 把节点搬出来：`Node` 不是 `Clone`（它带着
            // 网格、材质、物理组件），但可以整个换成一个空节点。
            let taken = std::mem::take(node);
            let new = self.nodes.spawn(taken);
            remap.insert(*old, new);
        }

        let translate = |handle: Handle<Node>| -> Handle<Node> {
            remap.get(&handle).copied().unwrap_or(Handle::NONE)
        };

        // ── 第二趟：把所有存着句柄的字段改过来 ──
        for new in remap.values().copied() {
            let Ok(node) = self.nodes.try_borrow_mut(new) else {
                continue;
            };

            node.parent = translate(node.parent);
            for child in &mut node.children {
                *child = translate(*child);
            }
            // 来源里已经失效的引用会变成 `Handle::NONE`，顺手清掉，
            // 免得留下一堆指向空的子节点。
            node.children.retain(|child| child.is_some());

            if let Some(skin) = node.skin.as_deref_mut() {
                skin.remap_joints(&translate);
            }
            if let Some(joint) = node.joint.as_deref_mut() {
                let (body1, body2) = (translate(joint.body1()), translate(joint.body2()));
                joint.set_bodies(body1, body2);
            }
        }

        // ── 接上：来源的根挂到指定父节点下 ──
        let new_root = translate(source_root);
        if new_root.is_none() {
            return Handle::NONE;
        }
        self.nodes[new_root].parent = parent;
        self.nodes[parent].children.push(new_root);

        for new in remap.values().copied() {
            self.index_physics_components(new);
        }

        new_root
    }

    /// 沿树自上而下重算所有节点的世界变换与可见性，并刷新剔除加速结构。
    ///
    /// 引擎每帧在插件的 `update` 之后自动调用，通常不需要手动调。
    pub fn update(&mut self) {
        self.culling.begin();
        self.index.clear();

        // ── 第一趟：沿树算世界变换，顺手把挂了组件的节点分类记下 ──
        let mut stack = vec![(self.root, Mat4::IDENTITY, true)];
        while let Some((handle, parent_matrix, parent_visible)) = stack.pop() {
            let (global, visible, child_count, drawable, components) = {
                let node = &mut self.nodes[handle];
                let global = parent_matrix * node.transform.matrix();
                let visible = parent_visible && node.visible;
                node.global_transform = global;
                node.global_visible = visible;
                (
                    global,
                    visible,
                    node.children.len(),
                    visible && node.mesh.is_some(),
                    (
                        node.light.is_some(),
                        node.camera.is_some(),
                        node.particles.is_some(),
                        node.skin.is_some(),
                        node.animator.is_some(),
                        node.rigid_body.is_some(),
                        node.collider.is_some(),
                        node.joint.is_some(),
                        node.ragdoll.is_some(),
                        node.sound.is_some(),
                        node.script.is_some(),
                        node.terrain.is_some(),
                    ),
                )
            };

            let (
                has_light,
                has_camera,
                has_particles,
                has_skin,
                has_animator,
                has_body,
                has_collider,
                has_joint,
                has_ragdoll,
                has_sound,
                has_script,
                has_terrain,
            ) = components;
            if drawable {
                self.index.drawables.push(handle);
            }
            if has_light {
                self.index.lights.push(handle);
            }
            if has_camera {
                self.index.cameras.push(handle);
            }
            if has_particles {
                self.index.particles.push(handle);
            }
            if has_skin {
                self.index.skinned.push(handle);
            }
            if has_animator {
                self.index.animators.push(handle);
            }
            if has_body {
                self.index.rigid_bodies.push(handle);
            }
            if has_collider {
                self.index.colliders.push(handle);
            }
            if has_joint {
                self.index.joints.push(handle);
            }
            if has_ragdoll {
                self.index.ragdolls.push(handle);
            }
            if has_sound {
                self.index.sounds.push(handle);
            }
            if has_script {
                self.index.scripts.push(handle);
            }
            if has_terrain {
                self.index.terrains.push(handle);
            }

            // 按下标取子节点而不是克隆整个列表：每帧对上万个节点做一次分配，
            // 光是分配器就能吃掉可观的时间。倒序压栈，弹出时才是原本的顺序。
            for index in (0..child_count).rev() {
                let child = self.nodes[handle].children[index];
                stack.push((child, global, visible));
            }
        }

        // ── 地形 ──
        // 排在第一趟**之后**：它要靠索引找到地形节点，而索引正是第一趟建的。
        // 排在之前的话第一帧一块都处理不到，地形要到第二帧才出现。
        //
        // 新建的块子节点赶不上第一趟，所以 `update_terrains` 会把它们的
        // 世界变换与索引补上——不补的话新块要等下一帧才进剔除结构，
        // 表现为相机一动，远处的地形闪一下才补上。
        self.update_terrains();

        // ── 第二趟：骨骼矩阵 ──
        // 必须等整棵树的世界变换都算完：关节可能在树上任何位置，
        // 边遍历边算的话，排在蒙皮网格后面的关节还停在上一帧。
        self.update_skins();

        // ── 第三趟：包围盒与剔除结构 ──
        // 同样得等骨骼算完——蒙皮网格的包围盒是由关节位置定的。
        for position in 0..self.index.drawables.len() {
            let handle = self.index.drawables[position];
            let aabb = self.compute_bounds(handle);
            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.global_aabb = aabb;
            }
            self.culling.push(handle, aabb);
        }

        self.culling.commit();
    }

    /// 重算所有骨架的骨骼矩阵。
    fn update_skins(&mut self) {
        for position in 0..self.index.skinned.len() {
            let handle = self.index.skinned[position];
            // 先把骨架摘出来：算矩阵要读别的节点，摘出来就不用同时借用整个池子了。
            let Some(mut skin) = self
                .nodes
                .try_borrow_mut(handle)
                .ok()
                .and_then(|node| node.skin.take())
            else {
                continue;
            };

            skin.update(|joint| self.try_get(joint).map(Node::global_transform));

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.skin = Some(skin);
            }
        }
    }

    /// 算一个可绘制节点的世界包围盒。
    fn compute_bounds(&self, handle: Handle<Node>) -> Aabb {
        let Some(node) = self.try_get(handle) else {
            return Aabb::EMPTY;
        };
        let Some(mesh) = node.mesh.as_ref() else {
            return Aabb::EMPTY;
        };

        let Some(skin) = node.skin.as_deref() else {
            // 形变会把顶点推出绑定姿态的包围盒，按当前权重把范围撑开。
            return mesh
                .morphed_aabb(node.morph_weights())
                .transform(node.global_transform);
        };

        // 蒙皮网格的顶点由骨骼驱动，绑定姿态的包围盒动起来就不准了
        // ——角色一抬手就会被判定成不可见。改用关节的世界位置定范围，
        // 再按网格自身的尺寸放宽，把附着在骨头上的皮肉包进去。
        let mut bounds = Aabb::EMPTY;
        for &joint in skin.joints() {
            if let Some(joint) = self.try_get(joint) {
                bounds.expand(joint.global_position());
            }
        }
        if bounds.is_empty() {
            return mesh.aabb().transform(node.global_transform);
        }

        let padding = Vec3::splat(mesh.aabb().half_extents().max_element().max(0.01));
        Aabb::new(bounds.min - padding, bounds.max + padding)
    }

    /// 推进场景里所有动画播放器，并把姿态写进目标节点的局部变换。
    ///
    /// 必须在 [`Scene::update`] **之前**调用：它改的是局部变换，
    /// 世界变换要在之后才重算，顺序反了动画就慢一帧。
    pub fn tick_animations(&mut self, dt: f32) {
        for position in 0..self.index.animators.len() {
            let handle = self.index.animators[position];
            // 同样先摘出来：应用姿态要写别的节点。
            let Some(mut player) = self
                .nodes
                .try_borrow_mut(handle)
                .ok()
                .and_then(|node| node.animator.take())
            else {
                continue;
            };

            player.animator_mut().tick(dt);

            for (target, entry) in player.pose().iter() {
                let node_handle = player.target(target);
                let Ok(node) = self.nodes.try_borrow_mut(node_handle) else {
                    continue;
                };
                // 没被驱动的分量保持节点原样，不能重置成单位值。
                if let Some(position) = entry.position {
                    node.transform.position = position;
                }
                if let Some(rotation) = entry.rotation {
                    node.transform.rotation = rotation;
                }
                if let Some(scale) = entry.scale {
                    node.transform.scale = scale;
                }
            }

            // 形变权重走另一张表：它是稀疏的，而且写的是网格上的权重数组，
            // 不是节点的局部变换。
            for sample in player.pose().morphs() {
                let node_handle = player.target(sample.target);
                let Ok(node) = self.nodes.try_borrow_mut(node_handle) else {
                    continue;
                };
                node.set_morph_weight(sample.index, sample.weight);
            }

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.animator = Some(player);
            }
        }
    }

    /// 参与剔除判定的对象数，即所有可见且带网格的节点数。
    pub fn drawable_count(&self) -> usize {
        self.culling.len()
    }

    /// 推进场景里所有粒子系统。
    ///
    /// 必须在 [`Scene::update`] **之后**调用：世界空间的粒子在出生时就要用到
    /// 节点的世界变换，变换没算就发射的话，第一批粒子会出现在原点。
    pub fn tick_particles(&mut self, dt: f32) {
        for position in 0..self.index.particles.len() {
            let handle = self.index.particles[position];
            let Ok(node) = self.nodes.try_borrow_mut(handle) else {
                continue;
            };
            // 不可见的系统冻结而不是继续烧 CPU；再次可见时从冻结处接着演。
            if !node.global_visible {
                continue;
            }
            let world = node.global_transform;
            let Some(mut system) = node.particles.take() else {
                continue;
            };

            system.tick(dt, world);
            // 开了场景碰撞的系统再走一遍射线检测。
            // kparticle 不认识物理引擎，这个洞由这里填上。
            self.resolve_particle_scene_collisions(&mut system, dt);

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.particles = Some(system);
            }
        }
    }

    /// 用物理世界的射线检测解决粒子与场景几何的碰撞。
    ///
    /// 粒子是**只受影响、不施加影响**的一方：它们不该把箱子撞开，
    /// 所以这里只做查询，一个字节的刚体状态都不改。
    fn resolve_particle_scene_collisions(&self, system: &mut ParticleSystem, dt: f32) {
        let Some(collision) = system.collision.as_ref() else {
            return;
        };
        if !collision.scene {
            return;
        }

        let physics = &self.physics;
        system.resolve_scene_collisions(dt, |from, to| {
            let delta = to - from;
            let distance = delta.length();
            // 这一帧没动的粒子不必查：射线长度为 0 时方向也无从谈起。
            if distance < 1e-6 {
                return None;
            }

            let hit = physics.cast_ray(&kphysics::RayCastOptions::new(
                from,
                delta / distance,
                distance,
            ))?;
            Some(kparticle::SurfaceHit {
                point: hit.point,
                normal: hit.normal,
            })
        });
    }

    /// 解一条双骨 IK 链（肩肘腕、胯膝踝都是这个结构），把结果写回局部旋转。
    ///
    /// 要在 [`Scene::update`] **之后**调用——求解读的是世界位置。
    /// 本链上的世界变换会就地刷新，所以紧接着读末端位置就是准的；
    /// 但包围盒与剔除结构要等下一次 `update` 才更新。
    /// 典型的一帧是：动画 → update → IK → 渲染。
    ///
    /// `target` 与 `pole` 都是世界坐标。返回是否真的解了（三个节点都有效才解）。
    pub fn solve_two_bone_ik(
        &mut self,
        root: Handle<Node>,
        mid: Handle<Node>,
        end: Handle<Node>,
        target: Vec3,
        pole: Option<Vec3>,
        weight: f32,
    ) -> bool {
        let (Some(root_node), Some(mid_node), Some(end_node)) =
            (self.try_get(root), self.try_get(mid), self.try_get(end))
        else {
            return false;
        };

        let solution = kanim::solve_two_bone(
            root_node.global_position(),
            mid_node.global_position(),
            end_node.global_position(),
            target,
            pole,
        )
        .scaled(weight);

        // 顺序要紧：先转根关节，再转中关节。
        // 每转完一个都要把它的子树刷新一遍——中关节的旋转增量是世界空间的，
        // 换算到局部时要除掉父链的世界旋转，而根关节刚刚才动过。
        self.apply_world_rotation(root, solution.root);
        self.refresh_subtree(root);
        self.apply_world_rotation(mid, solution.mid);
        self.refresh_subtree(mid);
        true
    }

    /// 重算一棵子树的世界变换，不碰剔除结构。
    ///
    /// 给 IK 这类「改完局部变换要立刻读世界坐标」的场合用；
    /// 整场景的刷新仍然走 [`Scene::update`]。
    fn refresh_subtree(&mut self, handle: Handle<Node>) {
        let Some(node) = self.try_get(handle) else {
            return;
        };
        let parent = node.parent;
        let parent_matrix = self
            .try_get(parent)
            .map(|parent| parent.global_transform)
            .unwrap_or(Mat4::IDENTITY);

        let mut stack = vec![(handle, parent_matrix)];
        while let Some((handle, parent_matrix)) = stack.pop() {
            let (global, child_count) = {
                let Ok(node) = self.nodes.try_borrow_mut(handle) else {
                    continue;
                };
                let global = parent_matrix * node.transform.matrix();
                node.global_transform = global;
                (global, node.children.len())
            };
            for index in (0..child_count).rev() {
                let child = self.nodes[handle].children[index];
                stack.push((child, global));
            }
        }
    }

    /// 把一个**世界空间**的旋转增量施加到节点上。
    ///
    /// 节点存的是局部旋转，所以要把增量换算到父节点的空间里：
    /// `局部增量 = 父世界旋转⁻¹ × 世界增量 × 父世界旋转`。
    fn apply_world_rotation(&mut self, handle: Handle<Node>, delta: Quat) {
        let Some(node) = self.try_get(handle) else {
            return;
        };
        let parent = node.parent;
        let parent_rotation = self
            .try_get(parent)
            .map(|parent| parent.global_transform.to_scale_rotation_translation().1)
            .unwrap_or(Quat::IDENTITY);

        let local_delta = parent_rotation.inverse() * delta * parent_rotation;
        if let Ok(node) = self.nodes.try_borrow_mut(handle) {
            node.transform.rotation = (local_delta * node.transform.rotation).normalize();
        }
    }

    /// 遍历所有需要绘制的粒子系统，可选地做视锥剔除。
    ///
    /// 走线性遍历而不是 BVH：粒子系统通常只有几十个，
    /// 为它们再维护一棵树，维护开销比省下的判定还多。
    pub fn visible_particles(&self, frustum: Option<&Frustum>) -> Vec<ParticleItem<'_>> {
        self.index
            .particles
            .iter()
            .filter_map(|&handle| {
                let node = self.try_get(handle)?;
                let system = node.particles()?;
                if !node.global_visible || system.is_empty() {
                    return None;
                }

                let aabb = system.world_bounds(node.global_transform);
                if let Some(frustum) = frustum
                    && !frustum.intersects(&aabb)
                {
                    return None;
                }

                Some(ParticleItem {
                    system,
                    transform: node.global_transform,
                    aabb,
                })
            })
            .collect()
    }

    /// 视锥剔除，返回落在视锥内的绘制项。
    ///
    /// 走 BVH 而非逐个判定：视锥外的整棵子树一次跳过，视锥内的整棵子树一次接受。
    /// 对象数超过阈值时自动切到 ktask 分片并行，结果与串行完全一致。
    ///
    /// 结果依赖 [`Scene::update`] 维护的加速结构，务必在其之后调用。
    pub fn cull(&self, frustum: &Frustum) -> Vec<RenderItem<'_>> {
        let mut indices = Vec::new();
        self.culling.cull(frustum, &mut indices);

        indices
            .into_iter()
            .filter_map(|index| self.render_item(self.culling.handle(index)))
            .collect()
    }

    /// 把一个节点转成绘制项；节点没有网格时返回 [`None`]。
    fn render_item(&self, handle: Handle<Node>) -> Option<RenderItem<'_>> {
        let node = self.try_get(handle)?;
        node.mesh().map(|mesh| RenderItem {
            mesh,
            material: node.material(),
            transform: skinned_transform(node),
            aabb: node.global_aabb,
            skin: node.skin().map(Skin::matrices),
            morph_weights: node.morph_weights(),
        })
    }

    /// 场景中第一个启用且可见的相机，返回（世界变换, 相机参数）。
    pub fn active_camera(&self) -> Option<(Mat4, Camera)> {
        self.index.cameras.iter().find_map(|&handle| {
            let node = self.try_get(handle)?;
            match node.camera() {
                Some(camera) if camera.enabled && node.global_visible => {
                    Some((node.global_transform, *camera))
                }
                _ => None,
            }
        })
    }

    /// 所有可见网格的世界包围盒之并。
    ///
    /// 阴影贴图用它决定光空间投影的覆盖范围；场景为空时返回空包围盒。
    /// 取自剔除结构的根节点，是 O(1) 的。
    pub fn visible_bounds(&self) -> Aabb {
        self.culling.bounds()
    }

    /// 场景中第一盏投射阴影的光源，返回（光源, 世界变换）。
    pub fn shadow_caster(&self) -> Option<(&Light, Mat4)> {
        self.visible_lights().find(|(light, _)| light.cast_shadows)
    }

    /// 遍历场景中所有启用且可见的光源，返回（光源, 世界变换）。
    ///
    /// 数量超过 [`klight::MAX_LIGHTS`] 时由渲染器截断。
    pub fn visible_lights(&self) -> impl Iterator<Item = (&Light, Mat4)> {
        self.index.lights.iter().filter_map(|&handle| {
            let node = self.try_get(handle)?;
            node.light()
                .filter(|light| light.enabled && node.global_visible)
                .map(|light| (light, node.global_transform))
        })
    }

    /// 遍历所有需要绘制的节点，不做剔除。
    ///
    /// 需要视锥剔除时用 [`Scene::cull`]，它走 BVH，对象多时快得多。
    pub fn visible_meshes(&self) -> impl Iterator<Item = RenderItem<'_>> {
        self.nodes.iter().filter_map(|node| {
            node.mesh()
                .filter(|_| node.global_visible)
                .map(|mesh| RenderItem {
                    mesh,
                    material: node.material(),
                    transform: skinned_transform(node),
                    aabb: node.global_aabb,
                    skin: node.skin().map(Skin::matrices),
                    morph_weights: node.morph_weights(),
                })
        })
    }

    /// `ancestor` 是否为 `node` 的祖先。
    fn is_ancestor_of(&self, ancestor: Handle<Node>, node: Handle<Node>) -> bool {
        let mut current = node;
        while let Some(n) = self.try_get(current) {
            if n.parent == ancestor {
                return true;
            }
            current = n.parent;
        }
        false
    }

    /// 把节点从其父节点的子列表中摘掉（不改动节点自身的 `parent` 字段）。
    fn detach_from_parent(&mut self, handle: Handle<Node>) {
        let parent = self.nodes[handle].parent;
        if let Ok(parent_node) = self.nodes.try_borrow_mut(parent) {
            parent_node.children.retain(|&c| c != handle);
        }
    }

    // ───────────────────────── 物理 ─────────────────────────

    /// 挂了脚本的节点，由 [`update`](Self::update) 收集。
    ///
    /// `kscript` 每帧靠它找到该跑的脚本，不必扫整个节点池。
    pub fn script_nodes(&self) -> &[Handle<Node>] {
        &self.index.scripts
    }

    /// 可绘制节点（可见且带网格），按树的深度优先顺序排列。
    pub fn drawable_nodes(&self) -> &[Handle<Node>] {
        &self.index.drawables
    }

    /// 带光源的节点。
    pub fn light_nodes(&self) -> &[Handle<Node>] {
        &self.index.lights
    }

    /// 带相机的节点。
    pub fn camera_nodes(&self) -> &[Handle<Node>] {
        &self.index.cameras
    }

    /// 带蒙皮的节点。
    pub fn skinned_nodes(&self) -> &[Handle<Node>] {
        &self.index.skinned
    }

    /// 当前活动相机所在的节点，没有则为 `None`。
    ///
    /// 判定口径与 [`active_camera`](Self::active_camera) 完全一致，
    /// 两者必须同进同出，否则调试绘制会把正在用的相机也画出来。
    pub fn active_camera_node(&self) -> Option<Handle<Node>> {
        self.index.cameras.iter().copied().find(|&handle| {
            self.try_get(handle)
                .is_some_and(|node| node.camera().is_some_and(|c| c.enabled) && node.global_visible)
        })
    }

    /// 物理世界的只读引用。射线检测等查询走这里。
    pub fn physics(&self) -> &PhysicsWorld {
        &self.physics
    }

    /// 物理世界的可变引用，用来改重力、求解器参数。
    pub fn physics_mut(&mut self) -> &mut PhysicsWorld {
        &mut self.physics
    }

    /// 本帧提交的 2D 精灵。
    pub fn sprites(&self) -> &[ksprite::SpriteInstance] {
        &self.sprites
    }

    /// 提交一个 2D 精灵。渲染器每帧画完会清空。
    ///
    /// 走这条路的精灵会经过专用的 2D 批处理（排序 + 合批 + 实例化）；
    /// 挂在节点上的精灵走的仍然是 3D 管线。几万个精灵时差别很大。
    pub fn push_sprite(&mut self, sprite: ksprite::SpriteInstance) {
        self.sprites.push(sprite);
    }

    /// 清空本帧的精灵。由引擎在帧末调用。
    ///
    /// **只清实例，不清贴图**：贴图是长期资源，每帧重传的话
    /// 一张 1024² 的图集每帧要走 4 MB 带宽。
    pub fn clear_sprites(&mut self) {
        self.sprites.clear();
    }

    /// 登记一张 2D 精灵用的贴图。
    ///
    /// 没登记过的纹理 id，渲染器会**跳过**那一批——而不是用别的顶替
    /// （顶替会在画面上印出完全不相干的图）。
    pub fn register_sprite_texture(&mut self, texture: ktexture::Texture) {
        if self.sprite_textures.iter().any(|t| t.id() == texture.id()) {
            return;
        }
        self.sprite_textures.push(texture);
    }

    /// 已登记的精灵贴图。渲染器每帧扫一遍，新的就上传。
    pub fn sprite_textures(&self) -> &[ktexture::Texture] {
        &self.sprite_textures
    }

    /// 用一张 HDR 全景图当环境光，漫反射与镜面都换掉。
    ///
    /// 这一步会做**两次离线计算**：球谐投影（漫反射）和 GGX 预滤波
    /// （镜面）。后者在默认参数下要卷积几万个像素，**在加载时做一次，
    /// 不要每帧调**。
    pub fn set_environment_hdr(
        &mut self,
        image: &kpbr::hdr::HdrImage,
        settings: kpbr::prefilter::PrefilterSettings,
    ) {
        self.environment.set_hdr(image);
        self.prefiltered_environment = Some(std::sync::Arc::new(kpbr::prefilter::prefilter(
            image, settings,
        )));
        self.environment_version += 1;
    }

    /// 预滤波的环境 mip 链。没设过 HDR 时为 `None`。
    pub fn prefiltered_environment(&self) -> Option<&[kpbr::prefilter::PrefilteredLevel]> {
        self.prefiltered_environment.as_ref().map(|c| c.as_slice())
    }

    /// 环境图的版本号。每次换图递增。
    pub fn environment_version(&self) -> u64 {
        self.environment_version
    }

    /// 本帧的调试线缓冲。
    pub fn gizmos(&self) -> &Gizmos {
        &self.gizmos
    }

    /// 往调试线缓冲里画东西。
    ///
    /// 默认是关的，先 [`Gizmos::set_enabled`] 打开。渲染器每帧画完会清空，
    /// 所以要让一条线一直在，就每帧都画它。
    pub fn gizmos_mut(&mut self) -> &mut Gizmos {
        &mut self.gizmos
    }

    /// 沿父链算出一个节点当前的世界变换。
    ///
    /// 与 `node.global_transform()` 的区别是**新鲜度**：后者是上一次
    /// [`update`](Self::update) 的结果，而物理同步排在 `update` 之前，
    /// 这一帧里刚加进来的节点、刚被逻辑改过的变换在那里都还没体现。
    /// 代价是 O(树深)，而带物理组件的节点通常只有几十个，可以忽略。
    pub fn world_matrix(&self, handle: Handle<Node>) -> Mat4 {
        let Some(node) = self.try_get(handle) else {
            return Mat4::IDENTITY;
        };
        if node.parent.is_none() {
            node.transform.matrix()
        } else {
            self.world_matrix(node.parent) * node.transform.matrix()
        }
    }

    /// 从 `handle` 起沿父链往上找第一个带原生刚体的节点。
    fn nearest_body_node(&self, handle: Handle<Node>) -> Option<(Handle<Node>, BodyHandle)> {
        let mut current = handle;
        while let Some(node) = self.try_get(current) {
            if let Some(native) = node.rigid_body.as_ref().and_then(|b| b.native()) {
                return Some((current, native));
            }
            if node.parent.is_none() {
                break;
            }
            current = node.parent;
        }
        None
    }

    /// 推进物理模拟并与场景图双向同步。
    ///
    /// 必须排在 [`update`](Self::update) **之前**：它写的是节点的局部变换，
    /// 世界变换、骨骼矩阵、包围盒都要在之后才重算。顺序反了，物体会画在
    /// 上一帧的位置上。
    ///
    /// `dt` 应当是定值，理由见 [`PhysicsWorld::step`]。
    pub fn step_physics(&mut self, dt: f32) {
        // 未激活的布娃娃要先把刚体摆到骨骼上，这一步产生的变换随后由
        // `sync_to_physics` 推给运动学刚体。
        self.ragdolls_follow_bones();
        self.sync_to_physics();
        self.physics.step(dt);
        self.sync_from_physics();
        // 激活的布娃娃反过来，用刚才的模拟结果改写骨骼。
        self.ragdolls_drive_bones();
    }

    /// 布娃娃节点的句柄列表（供 `ragdoll` 模块使用）。
    pub(crate) fn ragdoll_handles(&self) -> &[Handle<Node>] {
        &self.index.ragdolls
    }

    /// 把布娃娃组件摘出来。
    ///
    /// 驱动布娃娃要同时读写树上别的节点，摘出来就不用一直借着这个节点。
    /// 与骨架、动画播放器的做法一致。
    pub(crate) fn take_ragdoll(&mut self, handle: Handle<Node>) -> Option<Box<ragdoll::Ragdoll>> {
        self.nodes
            .try_borrow_mut(handle)
            .ok()
            .and_then(|node| node.ragdoll.take())
    }

    /// 把摘出去的布娃娃放回节点。
    pub(crate) fn put_ragdoll(&mut self, handle: Handle<Node>, ragdoll: Box<ragdoll::Ragdoll>) {
        if let Ok(node) = self.nodes.try_borrow_mut(handle) {
            node.ragdoll = Some(ragdoll);
        }
    }

    /// 重新登记一个节点的物理组件。
    ///
    /// 节点建好之后才挂上组件时要调一次——索引是在 `add_node` 那一刻建的，
    /// 之后加的组件它不知道。
    pub fn reindex_physics(&mut self, handle: Handle<Node>) {
        self.index_physics_components(handle);
    }

    /// 场景图 → 物理：建缺失的原生对象，推送用户改过的属性与变换。
    fn sync_to_physics(&mut self) {
        self.sync_bodies_to_physics();
        self.sync_colliders_to_physics();
        self.sync_joints_to_physics();
        // 排队的冲量与力必须等碰撞体建完才能施加。
        //
        // 刚体的质量是**碰撞体按密度算出来的**：只有刚体、没有碰撞体时质量为 0，
        // 而 rapier 的 `apply_impulse` 是 `Δv = 冲量 × 质量倒数`——质量 0 时
        // 倒数也是 0，冲量被**静默吞掉**，既不报错也没有任何迹象。
        // 症状是「新生成的物体第一帧推不动」，第二帧起又正常，极难查。
        self.flush_body_actions();
    }

    /// 把各刚体排队的操作推给物理世界。
    fn flush_body_actions(&mut self) {
        for position in 0..self.index.rigid_bodies.len() {
            let handle = self.index.rigid_bodies[position];
            let Some(mut body) = self
                .nodes
                .try_borrow_mut(handle)
                .ok()
                .and_then(|node| node.rigid_body.take())
            else {
                continue;
            };

            body.flush(&mut self.physics);

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.rigid_body = Some(body);
            }
        }
    }

    fn sync_bodies_to_physics(&mut self) {
        for position in 0..self.index.rigid_bodies.len() {
            let handle = self.index.rigid_bodies[position];
            if self.try_get(handle).is_none() {
                continue;
            }
            let (world_position, world_rotation) =
                kphysics::pose_from_matrix(self.world_matrix(handle));

            // 先把组件摘出来：接下来既要读整棵树（算世界变换），
            // 又要改物理世界，同时借着节点池会过不了借用检查。
            let Some(mut body) = self
                .nodes
                .try_borrow_mut(handle)
                .ok()
                .and_then(|node| node.rigid_body.take())
            else {
                continue;
            };

            match body.native() {
                None => {
                    // 初次登场：位姿取自节点，其余取自描述。
                    let mut desc = body.desc().clone();
                    desc.position = world_position;
                    desc.rotation = world_rotation;
                    let native = self.physics.add_body(&desc, handle.encode_to_u128());
                    body.set_native(Some(native));
                }
                Some(native) => match body.body_type() {
                    // 静态与运动学刚体由场景图驱动。
                    RigidBodyType::Fixed => {
                        if let Some(mut native_body) = self.physics.body_mut(native) {
                            let (p, r) = native_body.pose();
                            // 没动就别碰：`body_mut` 会让 rapier 重算接触，
                            // 每帧无谓地设一次位置在大场景里很贵。
                            if p != world_position || r != world_rotation {
                                native_body.set_position(world_position, world_rotation, false);
                            }
                        }
                    }
                    RigidBodyType::KinematicPositionBased => {
                        if let Some(mut native_body) = self.physics.body_mut(native) {
                            // 走 next_kinematic，让引擎反推出速度，
                            // 挡路的动态物体才会被正确推开而不是被穿过去。
                            native_body.set_next_kinematic_position(world_position, world_rotation);
                        }
                    }
                    _ => {}
                },
            }

            // 排队的动作不在这里 flush，见 `sync_to_physics` 的注释。

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.rigid_body = Some(body);
            }
        }
    }

    fn sync_colliders_to_physics(&mut self) {
        for position in 0..self.index.colliders.len() {
            let handle = self.index.colliders[position];
            if self.try_get(handle).is_none() {
                continue;
            }

            let owner = self.nearest_body_node(handle);
            let parent_body = owner.map(|(_, native)| native);
            let node_world = self.world_matrix(handle);
            // 挂在刚体上的碰撞体，位姿是**相对刚体**的；独立碰撞体则是世界位姿。
            let relative = match owner {
                Some((body_node, _)) => self.world_matrix(body_node).inverse() * node_world,
                None => node_world,
            };

            let Some(mut collider) = self
                .nodes
                .try_borrow_mut(handle)
                .ok()
                .and_then(|node| node.collider.take())
            else {
                continue;
            };

            let shape_changed = collider.take_shape_dirty();
            let desc_changed = collider.take_desc_dirty();
            // 换了形状或换了所属刚体，都只能重建——rapier 没有「原地改父刚体」。
            let needs_rebuild =
                collider.native().is_none() || collider.bound_to() != parent_body || shape_changed;

            if needs_rebuild {
                if let Some(old) = collider.native_mut().take() {
                    self.physics.remove_collider(old);
                }
                let mut desc = collider.desc_ref().clone();
                // 用户在描述里写的偏移，叠在节点层级算出的相对位姿之上。
                let (p, r) = kphysics::pose_from_matrix(
                    relative * Mat4::from_rotation_translation(desc.rotation, desc.position),
                );
                desc.position = p;
                desc.rotation = r;

                *collider.native_mut() =
                    self.physics
                        .add_collider(&desc, parent_body, handle.encode_to_u128());
                collider.set_bound_to(parent_body);
            } else if let Some(native) = collider.native() {
                let desc = collider.desc_ref();
                let (p, r) = kphysics::pose_from_matrix(
                    relative * Mat4::from_rotation_translation(desc.rotation, desc.position),
                );
                let (friction, restitution, sensor, groups) = (
                    desc.friction,
                    desc.restitution,
                    desc.is_sensor,
                    desc.collision_groups,
                );
                if let Some(mut native_collider) = self.physics.collider_mut(native) {
                    if desc_changed {
                        native_collider.set_friction(friction);
                        native_collider.set_restitution(restitution);
                        native_collider.set_sensor(sensor);
                        native_collider.set_collision_groups(groups);
                    }
                    if parent_body.is_some() {
                        native_collider.set_offset(p, r);
                    } else {
                        // 没有刚体的碰撞体完全由场景图驱动，每帧跟随节点。
                        native_collider.set_position(p, r);
                    }
                }
            }

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                node.collider = Some(collider);
            }
        }
    }

    fn sync_joints_to_physics(&mut self) {
        for position in 0..self.index.joints.len() {
            let handle = self.index.joints[position];
            let Some(node) = self.try_get(handle) else {
                continue;
            };
            let Some(joint) = node.joint.as_deref() else {
                continue;
            };
            let (node1, node2) = (joint.body1(), joint.body2());
            let native1 = self
                .try_get(node1)
                .and_then(|n| n.rigid_body.as_ref())
                .and_then(|b| b.native());
            let native2 = self
                .try_get(node2)
                .and_then(|n| n.rigid_body.as_ref())
                .and_then(|b| b.native());

            let (dirty, already_built, desc, old) = {
                let Ok(node) = self.nodes.try_borrow_mut(handle) else {
                    continue;
                };
                let Some(joint) = node.joint.as_deref_mut() else {
                    continue;
                };
                let dirty = joint.take_dirty();
                (
                    dirty,
                    joint.native().is_some(),
                    joint.desc_ref().clone(),
                    if dirty {
                        joint.native_mut().take()
                    } else {
                        None
                    },
                )
            };

            if already_built && !dirty {
                continue;
            }
            let (Some(native1), Some(native2)) = (native1, native2) else {
                // 两端的刚体还没建出来。下一帧再试——关节比刚体晚一帧到位
                // 是正常的，节点的创建顺序不受约束。
                continue;
            };

            if let Some(old) = old {
                self.physics.remove_joint(old);
            }
            let new_native = self.physics.add_joint(native1, native2, &desc);
            if let Ok(node) = self.nodes.try_borrow_mut(handle)
                && let Some(joint) = node.joint.as_deref_mut()
            {
                *joint.native_mut() = Some(new_native);
            }
        }
    }

    /// 物理 → 场景图：把模拟结果写回节点的局部变换。
    fn sync_from_physics(&mut self) {
        for position in 0..self.index.rigid_bodies.len() {
            let handle = self.index.rigid_bodies[position];
            let Some(node) = self.try_get(handle) else {
                continue;
            };
            let Some(body) = node.rigid_body.as_deref() else {
                continue;
            };
            // 静态刚体是场景图驱动的，回写只会把它自己的输入再抄一遍。
            if body.body_type() == RigidBodyType::Fixed {
                continue;
            }
            let Some(native) = body.native() else {
                continue;
            };
            let Some(native_body) = self.physics.body(native) else {
                continue;
            };
            let (world_position, world_rotation) = native_body.pose();
            let (linvel, angvel, sleeping) = (
                native_body.linvel(),
                native_body.angvel(),
                native_body.is_sleeping(),
            );

            let parent = node.parent;
            // 刚体的位姿是世界空间的，节点的变换是相对父节点的，得换算回去。
            let local = if parent.is_none() {
                Mat4::from_rotation_translation(world_rotation, world_position)
            } else {
                self.world_matrix(parent).inverse()
                    * Mat4::from_rotation_translation(world_rotation, world_position)
            };
            let (_, rotation, translation) = local.to_scale_rotation_translation();

            if let Ok(node) = self.nodes.try_borrow_mut(handle) {
                // 缩放保持不动：物理不认识缩放，写回时不能把它抹掉。
                node.transform.position = translation;
                node.transform.rotation = rotation;
                if let Some(body) = node.rigid_body.as_deref_mut() {
                    body.read_back(linvel, angvel, sleeping);
                }
            }
        }
    }

    /// 打一条射线，返回最近命中的**节点**。
    ///
    /// 比 [`PhysicsWorld::cast_ray`] 多做的一件事，就是把碰撞体身上的用户数据
    /// 解回节点句柄——这正是它被塞进去的用途。
    pub fn cast_ray(&self, options: &kphysics::RayCastOptions) -> Option<SceneRayHit> {
        let hit = self.physics.cast_ray(options)?;
        let collider = Handle::<Node>::decode_from_u128(hit.collider_user_data);
        let body = Handle::<Node>::decode_from_u128(hit.body_user_data);

        Some(SceneRayHit {
            collider_node: self.nodes.is_valid_handle(collider).then_some(collider),
            body_node: self.nodes.is_valid_handle(body).then_some(body),
            point: hit.point,
            normal: hit.normal,
            distance: hit.distance,
        })
    }

    /// 上一次 [`step_physics`](Self::step_physics) 产生的碰撞事件。
    pub fn collision_events(&self) -> &[CollisionEvent] {
        self.physics.collision_events()
    }

    /// 把一次碰撞事件里的两个碰撞体解回节点句柄。
    ///
    /// 已被删掉的碰撞体会得到 `None`——「因为对方被销毁而结束接触」的事件里
    /// 这是常态，不是错误。
    pub fn collision_nodes(
        &self,
        event: &CollisionEvent,
    ) -> (Option<Handle<Node>>, Option<Handle<Node>>) {
        let resolve = |data: u128| {
            let handle = Handle::<Node>::decode_from_u128(data);
            self.nodes.is_valid_handle(handle).then_some(handle)
        };
        (resolve(event.user_data1), resolve(event.user_data2))
    }
}

impl Index<Handle<Node>> for Scene {
    type Output = Node;

    fn index(&self, handle: Handle<Node>) -> &Self::Output {
        &self.nodes[handle]
    }
}

impl IndexMut<Handle<Node>> for Scene {
    fn index_mut(&mut self, handle: Handle<Node>) -> &mut Self::Output {
        &mut self.nodes[handle]
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kmath::Vec3;

    #[test]
    fn child_inherits_parent_transform() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent").with_position(Vec3::new(10.0, 0.0, 0.0)));
        let child = scene.add_node_with_parent(
            Node::new("Child").with_position(Vec3::new(0.0, 5.0, 0.0)),
            parent,
        );

        scene.update();

        // 子节点的世界坐标 = 父节点位移 + 自身位移。
        assert_eq!(scene[child].global_position(), Vec3::new(10.0, 5.0, 0.0));
        assert_eq!(scene[parent].global_position(), Vec3::new(10.0, 0.0, 0.0));
    }

    #[test]
    fn parent_scale_applies_to_child() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent").with_scale(Vec3::splat(2.0)));
        let child = scene.add_node_with_parent(
            Node::new("Child").with_position(Vec3::new(1.0, 0.0, 0.0)),
            parent,
        );

        scene.update();

        assert_eq!(scene[child].global_position(), Vec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn invisible_parent_hides_subtree() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent").with_mesh(Mesh::cube()));
        scene.add_node_with_parent(Node::new("Child").with_mesh(Mesh::cube()), parent);

        scene.update();
        assert_eq!(scene.visible_meshes().count(), 2);

        // 隐藏父节点，子节点也应一并从渲染列表中消失。
        scene[parent].visible = false;
        scene.update();
        assert_eq!(scene.visible_meshes().count(), 0);
    }

    #[test]
    fn remove_node_drops_whole_subtree() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent"));
        let child = scene.add_node_with_parent(Node::new("Child"), parent);
        let grandchild = scene.add_node_with_parent(Node::new("Grandchild"), child);

        scene.remove_node(parent);

        assert!(scene.try_get(parent).is_none());
        assert!(scene.try_get(child).is_none());
        assert!(scene.try_get(grandchild).is_none());
        // 根节点的子列表里也不该残留悬空句柄。
        assert!(scene[scene.root()].children().is_empty());
    }

    #[test]
    fn root_cannot_be_removed() {
        let mut scene = Scene::new();
        let root = scene.root();

        scene.remove_node(root);

        assert!(scene.try_get(root).is_some());
    }

    #[test]
    fn link_nodes_moves_child_between_parents() {
        let mut scene = Scene::new();
        let a = scene.add_node(Node::new("A"));
        let b = scene.add_node(Node::new("B"));
        let child = scene.add_node_with_parent(Node::new("Child"), a);

        scene.link_nodes(child, b);

        assert_eq!(scene[child].parent(), b);
        assert!(scene[a].children().is_empty());
        assert_eq!(scene[b].children(), &[child]);
    }

    #[test]
    fn link_nodes_rejects_cycles() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent"));
        let child = scene.add_node_with_parent(Node::new("Child"), parent);

        // 把父节点挂到自己的子节点下会形成环，应当被拒绝。
        scene.link_nodes(parent, child);

        assert_eq!(scene[child].parent(), parent);
        assert_eq!(scene[parent].parent(), scene.root());
    }

    #[test]
    fn find_by_name_locates_node() {
        let mut scene = Scene::new();
        let handle = scene.add_node(Node::new("Player"));

        assert_eq!(scene.find_by_name("Player"), Some(handle));
        assert_eq!(scene.find_by_name("Enemy"), None);
    }

    /// 构造一个两级层次的模型：根节点带网格，子节点带偏移。
    fn two_level_model() -> Model {
        use kgltf::{MeshPart, ModelNode, NodeTransform};

        let meshes = vec![Mesh::cube()];
        let materials = vec![Material::standard()];
        let nodes = vec![
            ModelNode {
                name: "Root".to_string(),
                children: vec![1],
                parts: vec![MeshPart {
                    mesh: 0,
                    material: Some(0),
                }],
                ..Default::default()
            },
            ModelNode {
                name: "Child".to_string(),
                transform: NodeTransform {
                    position: Vec3::new(0.0, 3.0, 0.0),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];

        Model::new(meshes, materials, nodes, vec![0])
    }

    #[test]
    fn instantiating_model_rebuilds_hierarchy() {
        let mut scene = Scene::new();
        let model = two_level_model();

        let root = scene.instantiate_model(&model, scene.root());
        scene.update();

        assert_eq!(scene[root].name, "Root");
        assert!(scene[root].mesh().is_some());
        assert!(scene[root].material().is_some());

        // 子节点应当继承父节点变换。
        let child = scene[root].children()[0];
        assert_eq!(scene[child].name, "Child");
        assert_eq!(scene[child].global_position(), Vec3::new(0.0, 3.0, 0.0));
    }

    #[test]
    fn multi_root_model_gets_container_node() {
        use kgltf::ModelNode;

        let nodes = vec![
            ModelNode {
                name: "A".to_string(),
                ..Default::default()
            },
            ModelNode {
                name: "B".to_string(),
                ..Default::default()
            },
        ];
        let model = Model::new(Vec::new(), Vec::new(), nodes, vec![0, 1]);

        let mut scene = Scene::new();
        let root = scene.instantiate_model(&model, scene.root());

        // 多根模型收拢到一个容器节点下，调用方只拿到一个句柄。
        assert_eq!(scene[root].name, "Model");
        assert_eq!(scene[root].children().len(), 2);
    }

    #[test]
    fn multi_part_node_splits_into_children() {
        use kgltf::{MeshPart, ModelNode};

        // 一个节点带两块几何体（多材质），必须拆成两个子节点，
        // 因为单个 Node 只能持有一份网格。
        let model = Model::new(
            vec![Mesh::cube(), Mesh::plane(1.0)],
            vec![Material::standard()],
            vec![ModelNode {
                name: "Multi".to_string(),
                parts: vec![
                    MeshPart {
                        mesh: 0,
                        material: Some(0),
                    },
                    MeshPart {
                        mesh: 1,
                        material: None,
                    },
                ],
                ..Default::default()
            }],
            vec![0],
        );

        let mut scene = Scene::new();
        let root = scene.instantiate_model(&model, scene.root());
        scene.update();

        assert!(scene[root].mesh().is_none());
        assert_eq!(scene[root].children().len(), 2);
        assert_eq!(scene.visible_meshes().count(), 2);
    }

    #[test]
    fn empty_model_does_not_panic() {
        let model = Model::new(Vec::new(), Vec::new(), Vec::new(), Vec::new());

        let mut scene = Scene::new();
        let root = scene.instantiate_model(&model, scene.root());

        assert!(scene.try_get(root).is_some());
    }

    #[test]
    fn world_aabb_follows_node_transform() {
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(5.0, 0.0, 0.0)),
        );

        scene.update();

        let aabb = scene[node].global_aabb();
        assert_eq!(aabb.center(), Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(aabb.size(), Vec3::ONE);
    }

    #[test]
    fn world_aabb_inherits_parent_scale() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent").with_scale(Vec3::splat(4.0)));
        let child = scene.add_node_with_parent(Node::new("Child").with_mesh(Mesh::cube()), parent);

        scene.update();

        // 父节点放大 4 倍，子节点的世界包围盒也应放大。
        assert_eq!(scene[child].global_aabb().size(), Vec3::splat(4.0));
    }

    #[test]
    fn node_without_mesh_has_empty_aabb() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Empty"));

        scene.update();

        assert!(scene[node].global_aabb().is_empty());
    }

    /// 相机在 +Z 朝原点看的视锥。
    fn test_frustum() -> Frustum {
        let camera = Camera::default();
        let view = Mat4::look_at_rh(Vec3::new(0.0, 4.0, 20.0), Vec3::ZERO, Vec3::Y);
        Frustum::from_view_projection(camera.projection_matrix(16.0 / 9.0) * view)
    }

    #[test]
    fn cull_matches_linear_filtering() {
        let mut scene = Scene::new();
        let mesh = Mesh::cube();
        // 一片 30×30 的方块，大半落在视野外。
        for x in 0..30 {
            for z in 0..30 {
                scene.add_node(
                    Node::new(format!("Cube{x}_{z}"))
                        .with_mesh(mesh.clone())
                        .with_position(Vec3::new(x as f32 - 15.0, 0.0, z as f32 - 15.0)),
                );
            }
        }
        scene.update();

        let frustum = test_frustum();
        let linear = scene
            .visible_meshes()
            .filter(|item| frustum.intersects(&item.aabb))
            .count();
        let culled = scene.cull(&frustum);

        // BVH 只是加速手段，可见集必须与逐个判定完全一致。
        assert_eq!(culled.len(), linear);
        assert!(culled.len() < scene.drawable_count());
        assert_eq!(scene.drawable_count(), 900);
    }

    #[test]
    fn cull_tracks_moved_nodes() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("Cube").with_mesh(Mesh::cube()));
        scene.update();
        assert_eq!(scene.cull(&test_frustum()).len(), 1);

        // 挪到视野外后，剔除结构必须跟着更新，否则会画出根本看不见的东西。
        scene[node].transform.position = Vec3::new(0.0, 0.0, 900.0);
        scene.update();

        assert_eq!(scene.cull(&test_frustum()).len(), 0);
        assert_eq!(scene.drawable_count(), 1);
    }

    #[test]
    fn cull_drops_hidden_and_removed_nodes() {
        let mut scene = Scene::new();
        let a = scene.add_node(Node::new("A").with_mesh(Mesh::cube()));
        let b = scene.add_node(
            Node::new("B")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(2.0, 0.0, 0.0)),
        );
        scene.update();
        assert_eq!(scene.cull(&test_frustum()).len(), 2);

        scene[a].visible = false;
        scene.update();
        assert_eq!(scene.cull(&test_frustum()).len(), 1);

        scene.remove_node(b);
        scene.update();
        assert_eq!(scene.cull(&test_frustum()).len(), 0);
        assert_eq!(scene.drawable_count(), 0);
    }

    #[test]
    fn cull_carries_material_and_transform() {
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(Material::standard())
                .with_position(Vec3::new(1.0, 2.0, 3.0)),
        );
        scene.update();

        let items = scene.cull(&test_frustum());
        assert_eq!(items.len(), 1);
        assert!(items[0].material.is_some());
        assert_eq!(
            items[0].transform.to_scale_rotation_translation().2,
            Vec3::new(1.0, 2.0, 3.0)
        );
    }

    /// 一个不受力、寿命够长的粒子系统，便于观察位置。
    fn test_particles() -> ParticleSystem {
        use kparticle::Emitter;

        ParticleSystem::new(
            Emitter::default()
                .with_rate(60.0)
                .with_lifetime(10.0)
                .with_speed(0.0)
                .with_size(0.2),
        )
        .with_seed(1)
    }

    #[test]
    fn particles_tick_and_are_collected() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("Smoke").with_particles(test_particles()));

        scene.update();
        // 没推进时一个粒子都没有，自然也收集不到。
        assert!(scene.visible_particles(None).is_empty());

        scene.tick_particles(0.5);

        let items = scene.visible_particles(None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].system.alive(), 30);
    }

    #[test]
    fn world_space_particles_spawn_at_the_node() {
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("Sparks")
                .with_particles(test_particles())
                .with_position(Vec3::new(7.0, 0.0, 0.0)),
        );

        scene.update();
        scene.tick_particles(0.1);

        // 世界空间的粒子在出生时就该被搬到节点所在处，
        // 这也是 tick_particles 必须排在 update 之后的原因。
        let items = scene.visible_particles(None);
        assert!((items[0].aabb.center().x - 7.0).abs() < 0.5);
    }

    #[test]
    fn hidden_particle_systems_freeze() {
        let mut scene = Scene::new();
        let handle = scene.add_node(Node::new("Smoke").with_particles(test_particles()));

        scene.update();
        scene.tick_particles(0.5);
        let before = scene[handle].particles().unwrap().alive();

        scene[handle].visible = false;
        scene.update();
        scene.tick_particles(5.0);

        // 看不见就不该继续烧 CPU，也不该被收集。
        assert_eq!(scene[handle].particles().unwrap().alive(), before);
        assert!(scene.visible_particles(None).is_empty());
    }

    #[test]
    fn offscreen_particle_systems_are_culled() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("Near").with_particles(test_particles()));
        scene.add_node(
            Node::new("Far")
                .with_particles(test_particles())
                .with_position(Vec3::new(0.0, 0.0, 900.0)),
        );

        scene.update();
        scene.tick_particles(0.5);

        assert_eq!(scene.visible_particles(None).len(), 2);
        assert_eq!(scene.visible_particles(Some(&test_frustum())).len(), 1);
    }

    #[test]
    fn particles_do_not_enter_the_mesh_culling_structure() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("Smoke").with_particles(test_particles()));

        scene.update();
        scene.tick_particles(1.0);

        // 粒子不投影、也不走网格那条批处理路径，
        // 混进 BVH 只会白白撑大阴影范围。
        assert_eq!(scene.drawable_count(), 0);
        assert!(scene.visible_bounds().is_empty());
    }

    #[test]
    fn frustum_culls_offscreen_nodes() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("Near").with_mesh(Mesh::cube()));
        scene.add_node(
            Node::new("Far")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(0.0, 0.0, 900.0)),
        );

        scene.update();

        // 相机在 +Z 朝原点看：近处的可见，正后方 900 处的不可见。
        let camera = Camera::default();
        let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let frustum = Frustum::from_view_projection(camera.projection_matrix(1.0) * view);

        let visible = scene
            .visible_meshes()
            .filter(|item| frustum.intersects(&item.aabb))
            .count();

        assert_eq!(scene.visible_meshes().count(), 2);
        assert_eq!(visible, 1);
    }

    #[test]
    fn visible_bounds_covers_every_mesh() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("A").with_mesh(Mesh::cube()));
        scene.add_node(
            Node::new("B")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(10.0, 0.0, 0.0)),
        );

        scene.update();
        let bounds = scene.visible_bounds();

        assert_eq!(bounds.min.x, -0.5);
        assert_eq!(bounds.max.x, 10.5);
    }

    #[test]
    fn visible_bounds_ignores_hidden_meshes() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("A").with_mesh(Mesh::cube()));
        let hidden = scene.add_node(
            Node::new("B")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(100.0, 0.0, 0.0)),
        );
        scene[hidden].visible = false;

        scene.update();

        // 隐藏的物体不该把阴影范围撑大，否则阴影分辨率会被白白摊薄。
        assert_eq!(scene.visible_bounds().max.x, 0.5);
    }

    #[test]
    fn empty_scene_has_empty_bounds() {
        let mut scene = Scene::new();
        scene.update();

        assert!(scene.visible_bounds().is_empty());
    }

    #[test]
    fn shadow_caster_picks_the_flagged_light() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("Fill").with_light(Light::point(5.0)));
        scene.add_node(Node::new("Sun").with_light(Light::directional().with_shadows()));

        scene.update();

        let (light, _) = scene.shadow_caster().expect("应当找到投影光源");
        assert!(light.cast_shadows);
    }

    #[test]
    fn no_shadow_caster_when_none_flagged() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("Fill").with_light(Light::point(5.0)));
        scene.update();

        assert!(scene.shadow_caster().is_none());
    }

    #[test]
    fn visible_lights_skips_disabled_ones() {
        let mut scene = Scene::new();
        let mut off = Light::point(5.0);
        off.enabled = false;
        scene.add_node(Node::new("Off").with_light(off));
        scene.add_node(Node::new("On").with_light(Light::point(5.0)));

        scene.update();

        assert_eq!(scene.visible_lights().count(), 1);
    }

    #[test]
    fn invisible_parent_hides_child_light() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Parent"));
        scene.add_node_with_parent(Node::new("Lamp").with_light(Light::point(5.0)), parent);

        scene.update();
        assert_eq!(scene.visible_lights().count(), 1);

        // 隐藏父节点，子节点上的光源也应停止参与照明。
        scene[parent].visible = false;
        scene.update();
        assert_eq!(scene.visible_lights().count(), 0);
    }

    #[test]
    fn light_transform_follows_node_hierarchy() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("Rig").with_position(Vec3::new(10.0, 0.0, 0.0)));
        scene.add_node_with_parent(
            Node::new("Lamp")
                .with_light(Light::point(5.0))
                .with_position(Vec3::new(0.0, 4.0, 0.0)),
            parent,
        );

        scene.update();

        let (light, transform) = scene.visible_lights().next().expect("应当有一盏光源");
        let gpu = light.to_gpu(transform);

        // 世界坐标 = 父节点位移 + 自身位移。
        assert_eq!(
            [gpu.position[0], gpu.position[1], gpu.position[2]],
            [10.0, 4.0, 0.0]
        );
    }

    #[test]
    fn active_camera_skips_disabled_ones() {
        let mut scene = Scene::new();
        let disabled = Camera {
            enabled: false,
            ..Default::default()
        };
        scene.add_node(Node::new("OffCamera").with_camera(disabled));
        let on = scene.add_node(
            Node::new("OnCamera")
                .with_camera(Camera::default())
                .with_position(Vec3::new(0.0, 0.0, 7.0)),
        );

        scene.update();

        let (camera_to_world, _) = scene.active_camera().expect("应当找到启用的相机");
        assert_eq!(
            camera_to_world.to_scale_rotation_translation().2,
            scene[on].global_position()
        );
    }

    // ── 骨骼动画（用仓库里的 Soldier.glb 做集成测试）──

    /// 加载 Soldier.glb：49 关节 + Idle/Run/TPose/Walk 四个动画。
    fn soldier() -> kasset::Resource<Model> {
        use kasset::{MemoryResourceIo, ResourceManager};
        use std::sync::Arc;

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/Soldier.glb");
        let bytes = std::fs::read(path).expect("仓库里应当有 assets/Soldier.glb");

        let mut io = MemoryResourceIo::new();
        io.add("Soldier.glb", bytes);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(kgltf::GltfLoader);
        manager
            .request_blocking::<Model>("Soldier.glb")
            .expect("Soldier.glb 应当能加载")
    }

    /// 把 Soldier 实例化进一个新场景，返回（场景，模型根节点）。
    fn soldier_scene() -> (Scene, Handle<Node>) {
        let model = soldier();
        let model = model.data_ref().unwrap();
        let mut scene = Scene::new();
        let root = scene.instantiate_model(&model, scene.root());
        scene.update();
        (scene, root)
    }

    /// 找到场景里第一个蒙皮网格节点。
    fn find_skinned(scene: &Scene) -> Handle<Node> {
        scene
            .nodes()
            .pair_iter()
            .find(|(_, node)| node.skin().is_some())
            .map(|(handle, _)| handle)
            .expect("应当有蒙皮网格节点")
    }

    #[test]
    fn instantiating_a_skinned_model_attaches_skins() {
        let (scene, _) = soldier_scene();

        let handle = find_skinned(&scene);
        let node = &scene[handle];
        let skin = node.skin().unwrap();

        // 网格与骨架必须落在同一个节点上，否则渲染时取不到骨骼矩阵。
        assert!(node.mesh().is_some());
        assert_eq!(skin.len(), 49);
        // 关节句柄都要指向真实存在的节点。
        assert!(skin.joints().iter().all(|&j| scene.try_get(j).is_some()));
    }

    #[test]
    fn instantiating_a_skinned_model_attaches_an_animator() {
        let (scene, root) = soldier_scene();

        let player = scene[root].animator().expect("模型根节点应当挂上播放器");
        let names: Vec<&str> = player
            .animator()
            .clips()
            .iter()
            .map(|clip| clip.name())
            .collect();

        assert_eq!(names, vec!["Idle", "Run", "TPose", "Walk"]);
        // 目标映射要覆盖模型的全部节点。
        assert_eq!(player.targets().len(), 68);
    }

    #[test]
    fn playing_an_animation_moves_the_joints() {
        let (mut scene, root) = soldier_scene();
        let joint = scene[find_skinned(&scene)].skin().unwrap().joints()[10];
        let before = scene[joint].transform;

        scene[root]
            .animator_mut()
            .unwrap()
            .animator_mut()
            .play_by_name("Walk")
            .expect("应当有 Walk 动画");
        scene.tick_animations(0.4);
        scene.update();

        // 走路动画必须真的把骨头转起来。
        assert_ne!(scene[joint].transform, before);
    }

    #[test]
    fn skin_matrices_are_finite_and_follow_the_animation() {
        let (mut scene, root) = soldier_scene();
        let handle = find_skinned(&scene);

        scene[root]
            .animator_mut()
            .unwrap()
            .animator_mut()
            .play_by_name("Run")
            .unwrap();
        scene.tick_animations(0.3);
        scene.update();
        let running = scene[handle].skin().unwrap().matrices().to_vec();

        assert_eq!(running.len(), 49);
        assert!(running.iter().all(|m| m.is_finite()), "骨骼矩阵出现了 NaN");

        // 再推进一段时间，姿态应当继续变化。
        scene.tick_animations(0.3);
        scene.update();
        let later = scene[handle].skin().unwrap().matrices();
        assert!(
            running.iter().zip(later).any(|(a, b)| a != b),
            "动画推进了，骨骼矩阵却纹丝不动"
        );
    }

    #[test]
    fn animation_is_deterministic() {
        let run = |seconds: f32| {
            let (mut scene, root) = soldier_scene();
            scene[root]
                .animator_mut()
                .unwrap()
                .animator_mut()
                .play_by_name("Walk")
                .unwrap();
            for _ in 0..(seconds * 60.0) as u32 {
                scene.tick_animations(1.0 / 60.0);
            }
            scene.update();
            scene[find_skinned(&scene)]
                .skin()
                .unwrap()
                .matrices()
                .to_vec()
        };

        // 同样的时间推进必须给出逐位相同的骨骼矩阵。
        assert_eq!(run(0.5), run(0.5));
    }

    #[test]
    fn skinned_meshes_render_with_an_identity_transform() {
        let (scene, _) = soldier_scene();

        let item = scene
            .visible_meshes()
            .find(|item| item.skin.is_some())
            .expect("应当收集到蒙皮绘制项");

        // 模型位姿在骨骼矩阵里，节点自身的变换按 glTF 规范要被忽略。
        assert_eq!(item.transform, Mat4::IDENTITY);
        assert_eq!(item.skin.unwrap().len(), 49);
    }

    #[test]
    fn skinned_bounds_follow_the_joints() {
        let (mut scene, root) = soldier_scene();
        let handle = find_skinned(&scene);

        // 把整个模型搬走，包围盒必须跟着走——绑定姿态的包围盒是不会动的。
        let bind_pose = scene[handle].global_aabb();
        scene[root].transform.position = Vec3::new(50.0, 0.0, 0.0);
        scene.update();
        let moved = scene[handle].global_aabb();

        assert!(
            (moved.center().x - bind_pose.center().x - 50.0).abs() < 1.0,
            "蒙皮包围盒没跟着模型走：{:?} → {:?}",
            bind_pose.center(),
            moved.center()
        );
    }

    #[test]
    fn skinned_bounds_contain_every_joint() {
        let (mut scene, root) = soldier_scene();
        scene[root]
            .animator_mut()
            .unwrap()
            .animator_mut()
            .play_by_name("Run")
            .unwrap();
        scene.tick_animations(0.5);
        scene.update();

        let handle = find_skinned(&scene);
        let bounds = scene[handle].global_aabb();
        for &joint in scene[handle].skin().unwrap().joints() {
            let position = scene[joint].global_position();
            assert!(
                bounds.contains(position),
                "关节 {position:?} 落在包围盒 {bounds:?} 之外，动起来就会被误剔除"
            );
        }
    }

    #[test]
    fn multiple_instances_share_clips_but_not_progress() {
        let model = soldier();
        let model = model.data_ref().unwrap();
        let mut scene = Scene::new();
        let first = scene.instantiate_model(&model, scene.root());
        let second = scene.instantiate_model(&model, scene.root());
        scene.update();

        // 两个实例各播各的。
        scene[first]
            .animator_mut()
            .unwrap()
            .animator_mut()
            .play_by_name("Walk")
            .unwrap();
        scene[second]
            .animator_mut()
            .unwrap()
            .animator_mut()
            .play_by_name("Idle")
            .unwrap();
        scene.tick_animations(0.25);

        let first_time = scene[first].animator().unwrap().animator().states()[0].time();
        assert!((first_time - 0.25).abs() < 1e-5);

        // 各自的骨架也要指向自己的关节，不能串台。
        let skinned: Vec<Handle<Node>> = scene
            .nodes()
            .pair_iter()
            .filter(|(_, node)| node.skin().is_some())
            .map(|(handle, _)| handle)
            .collect();
        assert_eq!(skinned.len(), 4); // 两个实例 × （身体 + 面罩）
        assert_ne!(
            scene[skinned[0]].skin().unwrap().joints()[0],
            scene[skinned[2]].skin().unwrap().joints()[0]
        );
    }

    #[test]
    fn static_models_get_no_animator() {
        let mut scene = Scene::new();
        let model = two_level_model();

        let root = scene.instantiate_model(&model, scene.root());
        scene.update();

        assert!(scene[root].animator().is_none());
        assert!(scene.visible_meshes().all(|item| item.skin.is_none()));
    }

    // ── IK ──

    /// 造一条三节点的链：root →(0,1,0)→ mid →(0,1,0)→ end。
    fn ik_chain(scene: &mut Scene) -> (Handle<Node>, Handle<Node>, Handle<Node>) {
        let root = scene.add_node(Node::new("Root"));
        // 中间关节先往侧面偏一点，免得三点共线、弯曲平面退化。
        let mid = scene.add_node_with_parent(
            Node::new("Mid").with_position(Vec3::new(0.2, 1.0, 0.0)),
            root,
        );
        let end = scene.add_node_with_parent(
            Node::new("End").with_position(Vec3::new(-0.2, 1.0, 0.0)),
            mid,
        );
        scene.update();
        (root, mid, end)
    }

    #[test]
    fn ik_moves_the_end_effector_onto_the_target() {
        let mut scene = Scene::new();
        let (root, mid, end) = ik_chain(&mut scene);
        let target = Vec3::new(1.2, 1.0, 0.0);

        assert!(scene.solve_two_bone_ik(root, mid, end, target, None, 1.0));
        scene.update();

        let reached = scene[end].global_position();
        assert!(
            (reached - target).length() < 1e-3,
            "末端停在 {reached:?}，没够到 {target:?}"
        );
    }

    #[test]
    fn ik_preserves_bone_lengths() {
        let mut scene = Scene::new();
        let (root, mid, end) = ik_chain(&mut scene);
        let upper = (scene[mid].global_position() - scene[root].global_position()).length();
        let lower = (scene[end].global_position() - scene[mid].global_position()).length();

        scene.solve_two_bone_ik(root, mid, end, Vec3::new(0.5, 0.5, 0.8), None, 1.0);
        scene.update();

        // IK 只能转关节，不能把骨头拉长。
        let new_upper = (scene[mid].global_position() - scene[root].global_position()).length();
        let new_lower = (scene[end].global_position() - scene[mid].global_position()).length();
        assert!((new_upper - upper).abs() < 1e-4);
        assert!((new_lower - lower).abs() < 1e-4);
    }

    #[test]
    fn ik_weight_of_zero_changes_nothing() {
        let mut scene = Scene::new();
        let (root, mid, end) = ik_chain(&mut scene);
        let before = scene[end].global_position();

        scene.solve_two_bone_ik(root, mid, end, Vec3::new(2.0, 0.0, 0.0), None, 0.0);
        scene.update();

        assert!((scene[end].global_position() - before).length() < 1e-5);
    }

    #[test]
    fn ik_works_under_a_transformed_parent() {
        // 整条链挂在一个被旋转和平移过的父节点下：
        // 求解在世界空间进行，写回的却是局部旋转，换算错了这里就会露馅。
        let mut scene = Scene::new();
        let rig = scene.add_node(
            Node::new("Rig")
                .with_position(Vec3::new(5.0, 0.0, -3.0))
                .with_transform(Transform {
                    position: Vec3::new(5.0, 0.0, -3.0),
                    rotation: Quat::from_rotation_y(1.1),
                    scale: Vec3::ONE,
                }),
        );
        let root = scene.add_node_with_parent(Node::new("Root"), rig);
        let mid = scene.add_node_with_parent(
            Node::new("Mid").with_position(Vec3::new(0.2, 1.0, 0.0)),
            root,
        );
        let end = scene.add_node_with_parent(
            Node::new("End").with_position(Vec3::new(-0.2, 1.0, 0.0)),
            mid,
        );
        scene.update();

        let target = scene[root].global_position() + Vec3::new(1.0, 1.0, 0.5);
        scene.solve_two_bone_ik(root, mid, end, target, None, 1.0);
        scene.update();

        let reached = scene[end].global_position();
        assert!(
            (reached - target).length() < 1e-3,
            "父节点带旋转时解错了：末端在 {reached:?}，目标 {target:?}"
        );
    }

    #[test]
    fn ik_rejects_invalid_handles() {
        let mut scene = Scene::new();
        let (root, mid, _) = ik_chain(&mut scene);

        assert!(!scene.solve_two_bone_ik(root, mid, Handle::NONE, Vec3::ONE, None, 1.0));
    }

    // ── 形变目标（用仓库里的 lion.glb 做集成测试）──

    /// 加载 lion.glb：4 个网格各带一个形变（mouth / leftEye / rightEye / tongue）。
    fn lion() -> kasset::Resource<Model> {
        use kasset::{MemoryResourceIo, ResourceManager};
        use std::sync::Arc;

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/lion.glb");
        let bytes = std::fs::read(path).expect("仓库里应当有 assets/lion.glb");

        let mut io = MemoryResourceIo::new();
        io.add("lion.glb", bytes);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(kgltf::GltfLoader);
        manager
            .request_blocking::<Model>("lion.glb")
            .expect("lion.glb 应当能加载")
    }

    /// 实例化 lion，返回（场景，带 mouth 形变的节点）。
    fn lion_scene() -> (Scene, Handle<Node>) {
        let model = lion();
        let model = model.data_ref().unwrap();
        let mut scene = Scene::new();
        scene.instantiate_model(&model, scene.root());
        scene.update();

        let mouth = scene
            .nodes()
            .pair_iter()
            .find(|(_, node)| node.find_morph_target("mouth").is_some())
            .map(|(handle, _)| handle)
            .expect("应当有带 mouth 形变的节点");
        (scene, mouth)
    }

    #[test]
    fn instantiating_a_morphed_model_seeds_the_default_weights() {
        let (scene, mouth) = lion_scene();

        // mouth 的默认权重是 1，实例化时要照搬过来。
        assert_eq!(scene[mouth].morph_weights(), &[1.0]);
        assert_eq!(scene[mouth].find_morph_target("mouth"), Some(0));
    }

    #[test]
    fn morph_weights_reach_the_render_item() {
        let (mut scene, mouth) = lion_scene();
        scene[mouth].set_morph_weight(0, 0.25);
        scene.update();

        let item = scene
            .visible_meshes()
            .find(|item| item.mesh.has_morph_targets())
            .expect("应当收集到带形变的绘制项");

        assert_eq!(
            scene
                .visible_meshes()
                .filter(|item| !item.morph_weights.is_empty())
                .count(),
            4
        );
        assert!(item.mesh.morph_target_count() > 0);
    }

    #[test]
    fn setting_a_morph_weight_by_name() {
        let (mut scene, mouth) = lion_scene();

        assert!(scene[mouth].set_morph_weight_by_name("mouth", 0.5));
        assert_eq!(scene[mouth].morph_weights(), &[0.5]);

        // 名字不存在时如实返回 false，而不是悄悄改错一个。
        assert!(!scene[mouth].set_morph_weight_by_name("nose", 1.0));
        assert_eq!(scene[mouth].morph_weights(), &[0.5]);
    }

    #[test]
    fn out_of_range_morph_index_is_ignored() {
        let (mut scene, mouth) = lion_scene();

        scene[mouth].set_morph_weight(99, 1.0);

        assert_eq!(scene[mouth].morph_weights().len(), 1);
    }

    #[test]
    fn morph_weight_changes_the_bounds() {
        let (mut scene, mouth) = lion_scene();
        scene[mouth].set_morph_weight(0, 0.0);
        scene.update();
        let rest = scene[mouth].global_aabb();

        scene[mouth].set_morph_weight(0, 1.0);
        scene.update();
        let open = scene[mouth].global_aabb();

        // 形变把顶点推出了绑定姿态的范围，包围盒必须跟着长大，
        // 否则张嘴的那一刻会被误剔除。
        assert!(
            open.size().length() > rest.size().length(),
            "形变后包围盒没变大：{:?} → {:?}",
            rest.size(),
            open.size()
        );
    }

    #[test]
    fn morph_weights_are_per_instance() {
        let model = lion();
        let model = model.data_ref().unwrap();
        let mut scene = Scene::new();
        scene.instantiate_model(&model, scene.root());
        scene.instantiate_model(&model, scene.root());
        scene.update();

        let mouths: Vec<Handle<Node>> = scene
            .nodes()
            .pair_iter()
            .filter(|(_, node)| node.find_morph_target("mouth").is_some())
            .map(|(handle, _)| handle)
            .collect();
        assert_eq!(mouths.len(), 2);

        scene[mouths[0]].set_morph_weight(0, 0.0);

        // 网格是共享资源，但表情得各做各的。
        assert_eq!(scene[mouths[0]].morph_weights(), &[0.0]);
        assert_eq!(scene[mouths[1]].morph_weights(), &[1.0]);
    }

    #[test]
    fn animation_drives_morph_weights() {
        use kanim::{AnimationClip, Animator, Channel, Curve, Interpolation, Track};
        use std::sync::Arc;

        // 合成一个只驱动形变权重的剪辑，挂到 lion 的某个节点上。
        let (mut scene, mouth) = lion_scene();
        let clip = AnimationClip::new(
            "Talk",
            vec![Track {
                target: 0,
                channel: Channel::MorphWeight {
                    index: 0,
                    curve: Curve::new(vec![0.0, 1.0], vec![0.0, 1.0], Interpolation::Linear)
                        .unwrap(),
                },
            }],
        );

        let mut animator = Animator::new(Arc::new(vec![clip]));
        animator.play(0).unwrap();
        let player = AnimationPlayer::new(animator, vec![mouth]);
        let rig = scene.add_node(Node::new("Rig").with_animator(player));
        let _ = rig;
        scene.update();

        scene.tick_animations(0.25);

        // 权重被动画写进了目标节点。
        assert_eq!(scene[mouth].morph_weights(), &[0.25]);
    }

    #[test]
    fn meshes_without_morph_targets_have_no_weights() {
        let mut scene = Scene::new();
        let cube = scene.add_node(Node::new("Cube").with_mesh(Mesh::cube()));
        scene.update();

        assert!(scene[cube].morph_weights().is_empty());
        assert!(
            scene
                .visible_meshes()
                .all(|item| item.morph_weights.is_empty())
        );
    }
}
