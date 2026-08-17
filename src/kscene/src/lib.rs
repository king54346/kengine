//! kscene —— 场景图。
//!
//! 一棵由 [`Node`] 组成的树，底层用 [`kcore::pool::Pool`] 存储。
//! 节点持有局部变换，世界变换与包围盒由 [`Scene::update`] 沿树自上而下算出。
//!
//! 本 crate **不依赖 wgpu**：渲染器通过 [`Scene::visible_meshes`] 拿到中立的
//! 绘制项列表，场景图本身不知道渲染后端的存在。

#![warn(missing_docs)]

mod cull;
mod node;
mod transform;

pub use kcamera::{Camera, Frustum, Projection};
pub use kmesh::{Mesh, Vertex};
pub use kmath::{Aabb, Intersection};
pub use node::Node;
pub use transform::Transform;

use cull::SceneCulling;
use kcore::pool::{Handle, Pool};
use kgltf::Model;
use kmaterial::Material;
use kmath::Mat4;
use klight::Light;
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
    pub transform: Mat4,
    /// 世界空间包围盒，用于剔除。
    pub aabb: Aabb,
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
        handle
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
            self.nodes.free(h);
        }
    }

    /// 把导入的 glTF 模型实例化成场景节点，返回新建子树的根。
    ///
    /// 模型有多个根节点时，会额外建一个容器节点把它们收拢，
    /// 这样调用方拿到的永远是单个句柄。
    pub fn instantiate_model(&mut self, model: &Model, parent: Handle<Node>) -> Handle<Node> {
        let roots = model.roots();

        // 单根模型直接实例化，不额外套一层。
        if let [only] = roots {
            return self.instantiate_model_node(model, *only, parent);
        }

        let container = self.add_node_with_parent(Node::new("Model"), parent);
        for &root in roots {
            self.instantiate_model_node(model, root, container);
        }
        container
    }

    fn instantiate_model_node(
        &mut self,
        model: &Model,
        index: usize,
        parent: Handle<Node>,
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
            self.instantiate_model_node(model, child, handle);
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

    /// 沿树自上而下重算所有节点的世界变换与可见性，并刷新剔除加速结构。
    ///
    /// 引擎每帧在插件的 `update` 之后自动调用，通常不需要手动调。
    pub fn update(&mut self) {
        self.culling.begin();

        let mut stack = vec![(self.root, Mat4::IDENTITY, true)];
        while let Some((handle, parent_matrix, parent_visible)) = stack.pop() {
            let (global, visible, child_count, drawable) = {
                let node = &mut self.nodes[handle];
                let global = parent_matrix * node.transform.matrix();
                let visible = parent_visible && node.visible;
                node.global_transform = global;
                node.global_visible = visible;
                // 世界包围盒与世界变换一同更新，剔除时直接取用。
                node.global_aabb = match node.mesh.as_ref() {
                    Some(mesh) => mesh.aabb().transform(global),
                    None => Aabb::EMPTY,
                };
                (
                    global,
                    visible,
                    node.children.len(),
                    visible && node.mesh.is_some(),
                )
            };

            if drawable {
                self.culling.push(handle, self.nodes[handle].global_aabb);
            }

            // 按下标取子节点而不是克隆整个列表：每帧对上万个节点做一次分配，
            // 光是分配器就能吃掉可观的时间。倒序压栈，弹出时才是原本的顺序。
            for index in (0..child_count).rev() {
                let child = self.nodes[handle].children[index];
                stack.push((child, global, visible));
            }
        }

        self.culling.commit();
    }

    /// 参与剔除判定的对象数，即所有可见且带网格的节点数。
    pub fn drawable_count(&self) -> usize {
        self.culling.len()
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
            transform: node.global_transform,
            aabb: node.global_aabb,
        })
    }

    /// 场景中第一个启用且可见的相机，返回（世界变换, 相机参数）。
    pub fn active_camera(&self) -> Option<(Mat4, Camera)> {
        self.nodes.iter().find_map(|node| match node.camera() {
            Some(camera) if camera.enabled && node.global_visible => {
                Some((node.global_transform, *camera))
            }
            _ => None,
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
        self.visible_lights()
            .find(|(light, _)| light.cast_shadows)
    }

    /// 遍历场景中所有启用且可见的光源，返回（光源, 世界变换）。
    ///
    /// 数量超过 [`klight::MAX_LIGHTS`] 时由渲染器截断。
    pub fn visible_lights(&self) -> impl Iterator<Item = (&Light, Mat4)> {
        self.nodes.iter().filter_map(|node| {
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
                    transform: node.global_transform,
                    aabb: node.global_aabb,
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
        scene.add_node(
            Node::new("Sun").with_light(Light::directional().with_shadows()),
        );

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
        assert_eq!([gpu.position[0], gpu.position[1], gpu.position[2]], [10.0, 4.0, 0.0]);
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





}
