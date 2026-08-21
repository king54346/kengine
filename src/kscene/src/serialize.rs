//! 场景序列化。
//!
//! # 文件里有什么
//!
//! ```text
//! Scene
//! ├── Version          格式版本，读到不认识的版本就直接报错
//! ├── Root             根节点句柄
//! ├── Meshes           共享网格表，按 id 去重
//! ├── Materials        共享材质表，按内容缓存键去重
//! ├── Nodes            节点池（不含网格与材质）
//! ├── NodeMeshes       节点 → 网格表下标
//! ├── NodeMaterials    节点 → 材质表下标
//! └── Environment
//! ```
//!
//! # 为什么网格和材质要单开一张表
//!
//! 一片一万个方块的场景里，那一万个节点共用同一份几何和同一份材质。
//! 逐节点内联的话文件会涨到几百 MB，读回来还会得到一万份互不相干的副本，
//! 渲染器的显存缓存与批处理全部失效。按 id 去重之后，几何只存一份，
//! 读回来也只有一份（[`Mesh`] 内部是 [`std::sync::Arc`] 共享的）。
//!
//! # 网格：能引用就不内联
//!
//! 从 glTF 导入的网格带着**出处**（[`kmesh::MeshSource`]），表里只写一行
//! 「`assets/Soldier.glb` 的第 3 个网格」，读回来时重新请求那个模型。
//! 程序化生成的网格（`Mesh::cube()` 之类）没有出处，只能把顶点内联进去。
//!
//! # 没有存进去的东西
//!
//! 粒子系统、动画播放器、布娃娃**不参与序列化**。它们要么是纯运行时状态
//! （粒子的存活列表、动画的播放进度），要么依赖资源里的数据（动画剪辑）。
//! 存盘时会照原样跳过，读回来的节点不带这些组件——不是悄悄丢失，
//! [`Scene::save`] 会在日志里说清楚跳过了几个。

use crate::{Collider, Joint, Node, RigidBody, Scene, Skin, Transform};
use kcore::{
    pool::{Handle, Pool},
    uuid::Uuid,
    visitor::{Visit, VisitResult, Visitor, error::VisitError},
};
use kgltf::Model;
use kmaterial::Material;
use kmesh::{Mesh, MeshSource};
use std::path::Path;

/// 场景文件的格式版本。
///
/// 格式一改就 +1。读到不认识的版本宁可直接报错，也不要按老规则解释新文件——
/// 那样得到的是一个「读进来了但哪儿都不对」的场景，比读不进来难查得多。
pub const SCENE_FORMAT_VERSION: u32 = 1;

/// 读写一个可选组件。
///
/// 比直接用 `Option<T>: Visit` 省一个 `T: Default` 约束——
/// 物理组件、骨架都没有有意义的「默认值」，硬造一个只会让接口变难懂。
fn visit_optional<T: Visit>(
    name: &str,
    slot: &mut Option<T>,
    visitor: &mut Visitor,
    make: impl FnOnce() -> T,
) -> VisitResult {
    let mut region = visitor.enter_region(name)?;

    let mut present = slot.is_some();
    present.visit("Present", &mut region)?;

    if present {
        if region.is_reading() {
            *slot = Some(make());
        }
        if let Some(value) = slot.as_mut() {
            value.visit("Value", &mut region)?;
        }
    } else if region.is_reading() {
        *slot = None;
    }

    Ok(())
}

impl Visit for Transform {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.position.visit("Position", &mut region)?;
        self.rotation.visit("Rotation", &mut region)?;
        self.scale.visit("Scale", &mut region)?;
        Ok(())
    }
}

impl Visit for Skin {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut joints = self.joints().to_vec();
        let mut inverse_bind = self.inverse_bind().to_vec();
        joints.visit("Joints", &mut region)?;
        inverse_bind.visit("InverseBind", &mut region)?;

        if region.is_reading() {
            // 骨骼矩阵是每帧由关节世界变换算出来的，不存；
            // 走 `new` 重建还能顺带拿到「两者数量不一致就截断」的保护。
            *self = Skin::new(joints, inverse_bind);
        }

        Ok(())
    }
}

impl Visit for RigidBody {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        // 只存描述。原生句柄、排队的动作、回读的速度都是运行时的东西，
        // 读回来是一个「还没进物理世界」的干净组件，下一次同步会把它建出来。
        self.desc_mut().visit("Desc", &mut region)?;
        Ok(())
    }
}

impl Visit for Collider {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.desc_mut().visit("Desc", &mut region)?;
        Ok(())
    }
}

impl Visit for Joint {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.desc_mut().visit("Desc", &mut region)?;

        let (mut body1, mut body2) = (self.body1(), self.body2());
        body1.visit("Body1", &mut region)?;
        body2.visit("Body2", &mut region)?;
        if region.is_reading() {
            self.set_bodies(body1, body2);
        }

        Ok(())
    }
}

impl Visit for Node {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.name.visit("Name", &mut region)?;
        self.transform.visit("Transform", &mut region)?;
        self.visible.visit("Visible", &mut region)?;
        self.morph_weights.visit("MorphWeights", &mut region)?;

        visit_optional("Camera", &mut self.camera, &mut region, Default::default)?;
        visit_optional("Light", &mut self.light, &mut region, Default::default)?;
        visit_optional("Skin", &mut self.skin, &mut region, || {
            Box::new(Skin::new(Vec::new(), Vec::new()))
        })?;
        visit_optional("RigidBody", &mut self.rigid_body, &mut region, || {
            Box::new(RigidBody::dynamic())
        })?;
        visit_optional("Collider", &mut self.collider, &mut region, || {
            Box::new(Collider::ball(0.5))
        })?;
        visit_optional("Joint", &mut self.joint, &mut region, || {
            Box::new(Joint::new(
                Handle::NONE,
                Handle::NONE,
                kphysics::JointDesc::default(),
            ))
        })?;

        self.parent.visit("Parent", &mut region)?;
        self.children.visit("Children", &mut region)?;

        // 网格与材质走共享表，见模块文档；世界变换、包围盒是每帧算出来的派生值。
        Ok(())
    }
}

/// 共享网格表里的一项。
///
/// 有出处就只写一行引用，没有才把顶点内联进去。
struct MeshEntry {
    id: Uuid,
    source: Option<MeshSource>,
    /// 内联的几何。有出处时为 [`None`]。
    inline: Option<Mesh>,
}

impl MeshEntry {
    fn from_mesh(mesh: &Mesh) -> Self {
        match mesh.source() {
            Some(source) => Self {
                id: mesh.id(),
                source: Some(source.clone()),
                inline: None,
            },
            None => Self {
                id: mesh.id(),
                source: None,
                inline: Some(mesh.clone()),
            },
        }
    }

    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.id.visit("Id", &mut region)?;

        let mut has_source = self.source.is_some();
        has_source.visit("HasSource", &mut region)?;

        if has_source {
            let mut source = self.source.take().unwrap_or_default();
            source.visit("Source", &mut region)?;
            self.source = Some(source);
            if region.is_reading() {
                self.inline = None;
            }
        } else {
            let mut mesh = self.inline.take().unwrap_or_default();
            mesh.visit("Inline", &mut region)?;
            self.inline = Some(mesh);
            if region.is_reading() {
                self.source = None;
            }
        }

        Ok(())
    }

    /// 把这一项还原成网格。
    ///
    /// 有出处的走资源管理器重新请求；请求不到时返回 `None`，
    /// 调用方会记一条日志并让那个节点不带网格——一个模型文件被挪走了，
    /// 不该让整个存档读不进来。
    fn resolve(&self, manager: Option<&kasset::ResourceManager>) -> Option<Mesh> {
        if let Some(mesh) = &self.inline {
            return Some(mesh.clone());
        }
        let source = self.source.as_ref()?;
        let manager = manager?;
        let model = manager.request_blocking::<Model>(&source.path).ok()?;
        let model = model.data_ref()?;
        let mesh = model.mesh(source.index)?.clone();
        Some(mesh)
    }
}

impl Scene {
    /// 把场景写进一个二进制文件。
    ///
    /// 顺带把用到的网格与材质各去重成一张表，见模块文档。
    pub fn save(&mut self, path: impl AsRef<Path>) -> VisitResult {
        let visitor = self.write_to_visitor()?;
        visitor.save_binary_to_file(path)
    }

    /// 把场景写成可读的 ASCII 文本，调试用。
    pub fn save_text(&mut self, path: impl AsRef<Path>) -> VisitResult {
        let visitor = self.write_to_visitor()?;
        visitor.save_ascii_to_file(path)
    }

    /// 序列化到字节，不落盘。
    pub fn save_to_vec(&mut self) -> Result<Vec<u8>, VisitError> {
        self.write_to_visitor()?.save_binary_to_vec()
    }

    /// 从文件读回一个场景。
    ///
    /// `manager` 用来解析资源引用（贴图、着色器、以及有出处的网格）。
    /// 传 `None` 也能读，但引用到外部资源的部分会缺失。
    pub fn load(
        path: impl AsRef<Path>,
        manager: Option<&kasset::ResourceManager>,
    ) -> Result<Scene, VisitError> {
        let bytes = std::fs::read(path).map_err(VisitError::Io)?;
        Self::load_from_slice(&bytes, manager)
    }

    /// 从字节读回一个场景。
    pub fn load_from_slice(
        bytes: &[u8],
        manager: Option<&kasset::ResourceManager>,
    ) -> Result<Scene, VisitError> {
        let mut visitor = Visitor::load_from_memory(bytes)?;
        if let Some(manager) = manager {
            visitor
                .blackboard
                .register(std::sync::Arc::new(manager.clone()));
        }
        Self::read_from_visitor(&mut visitor, manager)
    }

    fn write_to_visitor(&mut self) -> Result<Visitor, VisitError> {
        let mut visitor = Visitor::new();
        let mut region = visitor.enter_region("Scene")?;

        let mut version = SCENE_FORMAT_VERSION;
        version.visit("Version", &mut region)?;

        let mut root = self.root;
        root.visit("Root", &mut region)?;

        // ── 收集共享表 ──
        // 顺序按节点遍历序固定下来，同一个场景存两次得到同样的字节。
        let mut mesh_ids: Vec<Uuid> = Vec::new();
        let mut mesh_entries: Vec<MeshEntry> = Vec::new();
        let mut material_keys: Vec<(Uuid, u64)> = Vec::new();
        let mut material_entries: Vec<Material> = Vec::new();
        let mut node_meshes: Vec<(Handle<Node>, u32)> = Vec::new();
        let mut node_materials: Vec<(Handle<Node>, u32)> = Vec::new();
        // 不参与序列化的组件，统计一下好在日志里说清楚。
        let (mut skipped_particles, mut skipped_animators, mut skipped_ragdolls) = (0, 0, 0);

        for index in 0..self.nodes.get_capacity() {
            let handle = self.nodes.handle_from_index(index);
            let Some(node) = self.nodes.try_borrow(handle).ok() else {
                continue;
            };

            if let Some(mesh) = node.mesh() {
                let slot = match mesh_ids.iter().position(|id| *id == mesh.id()) {
                    Some(slot) => slot,
                    None => {
                        mesh_ids.push(mesh.id());
                        mesh_entries.push(MeshEntry::from_mesh(mesh));
                        mesh_ids.len() - 1
                    }
                };
                node_meshes.push((handle, slot as u32));
            }

            skipped_particles += usize::from(node.particles().is_some());
            skipped_animators += usize::from(node.animator().is_some());
            skipped_ragdolls += usize::from(node.ragdoll().is_some());

            if let Some(material) = node.material() {
                let key = material.cache_key();
                let slot = match material_keys.iter().position(|k| *k == key) {
                    Some(slot) => slot,
                    None => {
                        material_keys.push(key);
                        material_entries.push(material.clone());
                        material_keys.len() - 1
                    }
                };
                node_materials.push((handle, slot as u32));
            }
        }

        if skipped_particles + skipped_animators + skipped_ragdolls > 0 {
            // 明说，别让它变成「读回来怎么少了东西」的悬案。
            klog::warn!(
                "场景序列化跳过了不支持的组件：粒子 {skipped_particles} /                  动画播放器 {skipped_animators} / 布娃娃 {skipped_ragdolls}"
            );
        }

        // ── 写表 ──
        {
            let mut meshes = region.enter_region("Meshes")?;
            let mut count = mesh_entries.len() as u32;
            count.visit("Count", &mut meshes)?;
            for (index, entry) in mesh_entries.iter_mut().enumerate() {
                entry.visit(&format!("Mesh{index}"), &mut meshes)?;
            }
        }
        {
            let mut materials = region.enter_region("Materials")?;
            let mut count = material_entries.len() as u32;
            count.visit("Count", &mut materials)?;
            for (index, material) in material_entries.iter_mut().enumerate() {
                material.visit(&format!("Material{index}"), &mut materials)?;
            }
        }

        // ── 写节点池 ──
        // `Pool` 的 `Visit` 是现成的（kcore 里就有），句柄的世代号也一并存下来，
        // 于是读回来的句柄和存之前完全等价。
        self.nodes.visit("Nodes", &mut region)?;

        visit_pairs("NodeMeshes", &mut node_meshes, &mut region)?;
        visit_pairs("NodeMaterials", &mut node_materials, &mut region)?;

        self.environment.visit("Environment", &mut region)?;

        drop(region);
        Ok(visitor)
    }

    fn read_from_visitor(
        visitor: &mut Visitor,
        manager: Option<&kasset::ResourceManager>,
    ) -> Result<Scene, VisitError> {
        let mut region = visitor.enter_region("Scene")?;

        let mut version = 0u32;
        version.visit("Version", &mut region)?;
        if version != SCENE_FORMAT_VERSION {
            return Err(VisitError::User(format!(
                "场景文件版本是 {version}，本引擎只认 {SCENE_FORMAT_VERSION}"
            )));
        }

        let mut root = Handle::NONE;
        root.visit("Root", &mut region)?;

        let mut mesh_entries = Vec::new();
        {
            let mut meshes = region.enter_region("Meshes")?;
            let mut count = 0u32;
            count.visit("Count", &mut meshes)?;
            for index in 0..count {
                let mut entry = MeshEntry {
                    id: Uuid::nil(),
                    source: None,
                    inline: None,
                };
                entry.visit(&format!("Mesh{index}"), &mut meshes)?;
                mesh_entries.push(entry);
            }
        }

        let mut material_entries: Vec<Material> = Vec::new();
        {
            let mut materials = region.enter_region("Materials")?;
            let mut count = 0u32;
            count.visit("Count", &mut materials)?;
            for index in 0..count {
                let mut material = Material::new();
                material.visit(&format!("Material{index}"), &mut materials)?;
                material_entries.push(material);
            }
        }

        let mut nodes: Pool<Node> = Pool::new();
        nodes.visit("Nodes", &mut region)?;

        let mut node_meshes = Vec::new();
        let mut node_materials = Vec::new();
        visit_pairs("NodeMeshes", &mut node_meshes, &mut region)?;
        visit_pairs("NodeMaterials", &mut node_materials, &mut region)?;

        let mut environment = kpbr::Environment::default();
        environment.visit("Environment", &mut region)?;

        drop(region);

        // ── 把共享表接回节点 ──
        // 同一张表项被多个节点引用时，`Mesh` 的克隆是 O(1) 的（内部 Arc 共享），
        // 一万个方块读回来仍然只有一份几何。
        let mut resolved: Vec<Option<Mesh>> = Vec::with_capacity(mesh_entries.len());
        let mut missing = 0usize;
        for entry in &mesh_entries {
            let mesh = entry.resolve(manager);
            if mesh.is_none() {
                missing += 1;
                klog::warn!(
                    "场景里的网格引用解析失败：{:?}，相关节点将不带网格",
                    entry.source
                );
            }
            resolved.push(mesh);
        }
        if missing > 0 {
            klog::warn!("共有 {missing} 个网格引用没能解析");
        }

        for (handle, slot) in node_meshes {
            let Some(Some(mesh)) = resolved.get(slot as usize) else {
                continue;
            };
            if let Ok(node) = nodes.try_borrow_mut(handle) {
                // 直接赋值而不是走 `set_mesh`：那个会把形变权重重置成网格自带的
                // 默认值，而文件里存的是这个实例**当前**的表情，不能被盖掉。
                node.mesh = Some(mesh.clone());
                // 万一网格换过（形变目标数量变了），按目标数补齐或截断，
                // 已存下来的权重尽量留住。
                node.morph_weights.resize(mesh.morph_target_count(), 0.0);
            }
        }
        for (handle, slot) in node_materials {
            let Some(material) = material_entries.get(slot as usize) else {
                continue;
            };
            if let Ok(node) = nodes.try_borrow_mut(handle) {
                node.set_material(material.clone());
            }
        }

        let mut scene = Scene::from_parts(nodes, root, environment);
        // 世界变换、包围盒、剔除结构、组件索引都是派生数据，重新算一遍。
        scene.update();
        Ok(scene)
    }
}

/// 读写一串 `(节点句柄, 表下标)`。
fn visit_pairs(
    name: &str,
    pairs: &mut Vec<(Handle<Node>, u32)>,
    visitor: &mut Visitor,
) -> VisitResult {
    let mut region = visitor.enter_region(name)?;

    let mut count = pairs.len() as u32;
    count.visit("Count", &mut region)?;

    if region.is_reading() {
        pairs.clear();
        pairs.resize(count as usize, (Handle::NONE, 0));
    }

    for (index, (handle, slot)) in pairs.iter_mut().enumerate() {
        let mut entry = region.enter_region(&format!("Entry{index}"))?;
        handle.visit("Node", &mut entry)?;
        slot.visit("Slot", &mut entry)?;
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Camera, Collider, Light, Projection, RigidBody};
    use kasset::{MemoryResourceIo, ResourceManager};
    use kmath::{Mat4, Quat, Vec3, Vec4};
    use kmesh::Mesh;
    use kpbr::PbrMaterial;
    use kphysics::{ColliderShape, JointDesc, JointKind, RigidBodyType};
    use std::sync::Arc;

    fn manager() -> ResourceManager {
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new()));
        manager.add_loader(kgltf::GltfLoader);
        manager
    }

    /// 存盘再读回来。
    fn roundtrip(scene: &mut Scene) -> Scene {
        let bytes = scene.save_to_vec().expect("存盘失败");
        Scene::load_from_slice(&bytes, Some(&manager())).expect("读档失败")
    }

    #[test]
    fn an_empty_scene_survives_a_roundtrip() {
        let mut scene = Scene::new();
        let restored = roundtrip(&mut scene);

        assert_eq!(restored.root(), scene.root());
        assert_eq!(restored[restored.root()].name, "__root");
    }

    #[test]
    fn the_hierarchy_and_transforms_come_back_intact() {
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("parent").with_transform(Transform {
            position: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(0.7),
            scale: Vec3::splat(2.0),
        }));
        let child = scene.add_node_with_parent(
            Node::new("child").with_position(Vec3::new(0.0, 5.0, 0.0)),
            parent,
        );
        scene.update();
        let expected_world = scene[child].global_transform();

        let restored = roundtrip(&mut scene);

        assert_eq!(restored[child].name, "child");
        assert_eq!(restored[child].parent(), parent);
        assert!(restored[parent].children().contains(&child));
        assert_eq!(restored[parent].transform.scale, Vec3::splat(2.0));
        // 世界变换是派生数据，读回来应当由 `update` 重算出同一个结果。
        let restored_world = restored[child].global_transform();
        assert!(
            (restored_world - expected_world)
                .to_cols_array()
                .iter()
                .all(|v| v.abs() < 1e-5),
            "世界变换没对上：{restored_world:?} vs {expected_world:?}"
        );
    }

    #[test]
    fn visibility_survives() {
        let mut scene = Scene::new();
        let hidden = scene.add_node(Node::new("hidden").with_mesh(Mesh::cube()));
        scene[hidden].visible = false;

        let restored = roundtrip(&mut scene);

        assert!(!restored[hidden].visible);
        assert_eq!(restored.drawable_count(), 0, "隐藏的节点不该进绘制列表");
    }

    #[test]
    fn a_procedural_mesh_is_inlined_and_comes_back_whole() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("sphere").with_mesh(Mesh::sphere(8, 12)));
        let original = scene[node].mesh().unwrap().clone();

        let restored = roundtrip(&mut scene);

        let mesh = restored[node].mesh().expect("网格丢了");
        assert_eq!(mesh.id(), original.id(), "id 变了，显存缓存会失配");
        assert_eq!(mesh.vertices(), original.vertices());
        assert_eq!(mesh.indices(), original.indices());
    }

    #[test]
    fn nodes_sharing_a_mesh_still_share_it_after_loading() {
        // 这是共享表存在的理由：一万个方块读回来仍然只有一份几何。
        let mut scene = Scene::new();
        let mesh = Mesh::cube();
        let a = scene.add_node(Node::new("a").with_mesh(mesh.clone()));
        let b = scene.add_node(Node::new("b").with_mesh(mesh.clone()));
        let c = scene.add_node(Node::new("c").with_mesh(mesh));

        let restored = roundtrip(&mut scene);

        let ma = restored[a].mesh().unwrap();
        assert!(ma.shares_data_with(restored[b].mesh().unwrap()));
        assert!(ma.shares_data_with(restored[c].mesh().unwrap()));
    }

    #[test]
    fn a_shared_mesh_is_only_written_once() {
        // 直接量「再加一个节点要多花多少字节」：共享几何的话只多一行表项，
        // 各带各的几何则要多一整份顶点。比总大小的比值稳，也更说明问题。
        fn size(shared: bool, count: usize) -> usize {
            let mut scene = Scene::new();
            let mesh = Mesh::cube();
            for index in 0..count {
                let mesh = if shared { mesh.clone() } else { Mesh::cube() };
                scene.add_node(Node::new(format!("n{index}")).with_mesh(mesh));
            }
            scene.save_to_vec().unwrap().len()
        }

        let shared_marginal = size(true, 51) - size(true, 50);
        let own_marginal = size(false, 51) - size(false, 50);

        assert!(
            shared_marginal * 2 < own_marginal,
            "共享几何没有被去重：多一个共享节点 {shared_marginal} 字节，             多一个自带几何的节点 {own_marginal} 字节"
        );
    }

    #[test]
    fn materials_are_deduplicated_but_distinct_ones_are_kept_apart() {
        let mut scene = Scene::new();
        let shared = PbrMaterial::dielectric(Vec3::new(0.2, 0.4, 0.8), 0.3);
        let a = scene.add_node(
            Node::new("a")
                .with_mesh(Mesh::cube())
                .with_material(shared.clone()),
        );
        let b = scene.add_node(Node::new("b").with_mesh(Mesh::cube()).with_material(shared));
        let c = scene.add_node(
            Node::new("c")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::ONE, 0.1)),
        );

        let restored = roundtrip(&mut scene);

        assert_eq!(
            restored[a].material().unwrap().cache_key(),
            restored[b].material().unwrap().cache_key(),
            "同一份材质读回来该还是同一份"
        );
        assert_ne!(
            restored[a].material().unwrap().base_color(),
            restored[c].material().unwrap().base_color()
        );
    }

    #[test]
    fn material_parameters_survive() {
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("n").with_mesh(Mesh::cube()).with_material(
                Material::standard()
                    .with_base_color(Vec4::new(0.1, 0.2, 0.3, 0.4))
                    .with_metallic(0.8)
                    .with_roughness(0.15),
            ),
        );

        let restored = roundtrip(&mut scene);
        let material = restored[node].material().unwrap();

        assert_eq!(material.base_color(), Vec4::new(0.1, 0.2, 0.3, 0.4));
        assert_eq!(material.metallic(), 0.8);
        assert_eq!(material.roughness(), 0.15);
    }

    #[test]
    fn lights_survive_with_their_kind_and_parameters() {
        let mut scene = Scene::new();
        let directional = scene.add_node(Node::new("sun").with_light(Light {
            kind: klight::LightKind::Directional,
            color: Vec3::new(1.0, 0.9, 0.8),
            intensity: 4.5,
            enabled: true,
            cast_shadows: true,
        }));
        let spot = scene.add_node(Node::new("spot").with_light(Light {
            kind: klight::LightKind::Spot {
                range: 12.0,
                inner_angle: 15.0,
                outer_angle: 30.0,
            },
            intensity: 2.0,
            ..Default::default()
        }));

        let restored = roundtrip(&mut scene);

        let sun = restored[directional].light().unwrap();
        assert_eq!(sun.intensity, 4.5);
        assert!(sun.cast_shadows);
        assert_eq!(sun.color, Vec3::new(1.0, 0.9, 0.8));

        match restored[spot].light().unwrap().kind {
            klight::LightKind::Spot {
                range,
                inner_angle,
                outer_angle,
            } => {
                assert_eq!((range, inner_angle, outer_angle), (12.0, 15.0, 30.0));
            }
            other => panic!("聚光灯读成了 {other:?}"),
        }
    }

    #[test]
    fn cameras_survive_with_their_projection() {
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("cam").with_camera(Camera {
            projection: Projection::Orthographic { height: 7.5 },
            z_near: 0.5,
            z_far: 250.0,
            enabled: true,
            frustum_culling: false,
        }));

        let restored = roundtrip(&mut scene);
        let camera = restored[node].camera().unwrap();

        assert_eq!(camera.projection, Projection::Orthographic { height: 7.5 });
        assert_eq!(camera.z_near, 0.5);
        assert_eq!(camera.z_far, 250.0);
        assert!(!camera.frustum_culling);
        assert!(restored.active_camera().is_some());
    }

    #[test]
    fn the_environment_survives_and_its_harmonics_are_rebuilt() {
        let mut scene = Scene::new();
        scene.environment_mut().intensity = 0.35;
        scene.environment_mut().sky.zenith = Vec3::new(0.9, 0.1, 0.2);
        scene.environment_mut().rebuild();
        let expected = scene.environment().irradiance(Vec3::Y);

        let restored = roundtrip(&mut scene);

        assert_eq!(restored.environment().intensity, 0.35);
        assert_eq!(restored.environment().sky.zenith, Vec3::new(0.9, 0.1, 0.2));
        // 球谐系数不存盘，读回来是重算的——结果必须一致。
        let actual = restored.environment().irradiance(Vec3::Y);
        assert!(
            (actual - expected).length() < 1e-4,
            "{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn morph_weights_survive() {
        use kmesh::{MorphDelta, MorphTarget};

        let base = Mesh::plane(1.0);
        let count = base.vertices().len();
        let target = |name: &str, amount: f32| {
            MorphTarget::new(
                name,
                vec![
                    MorphDelta {
                        position: [0.0, amount, 0.0],
                        ..Default::default()
                    };
                    count
                ],
            )
        };
        let mesh =
            base.with_morph_targets(vec![target("a", 1.0), target("b", 2.0)], vec![0.0, 0.0]);

        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("face").with_mesh(mesh));
        scene[node].set_morph_weight(0, 0.25);
        scene[node].set_morph_weight(1, 0.75);

        let restored = roundtrip(&mut scene);

        assert_eq!(restored[node].morph_weights(), &[0.25, 0.75]);
        // 形变目标本身也要跟着网格回来，否则权重没有作用对象。
        assert_eq!(restored[node].mesh().unwrap().morph_target_count(), 2);
        assert_eq!(restored[node].find_morph_target("b"), Some(1));
    }

    #[test]
    fn physics_components_survive_as_descriptions() {
        let mut scene = Scene::new();
        let ground = scene.add_node(
            Node::new("ground")
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::cuboid(Vec3::new(10.0, 0.5, 10.0))),
        );
        let ball = scene.add_node(
            Node::new("ball")
                .with_position(Vec3::Y * 5.0)
                .with_rigid_body(RigidBody::new(
                    kphysics::RigidBodyDesc::dynamic()
                        .with_gravity_scale(0.5)
                        .with_locked_rotations(),
                ))
                .with_collider(Collider::new(
                    kphysics::ColliderDesc::ball(0.75).with_material(0.9, 0.3),
                )),
        );

        let mut restored = roundtrip(&mut scene);

        assert_eq!(
            restored[ground].rigid_body().unwrap().body_type(),
            RigidBodyType::Fixed
        );
        let body = restored[ball].rigid_body().unwrap();
        assert_eq!(body.desc().gravity_scale, 0.5);
        assert_eq!(body.desc().locked_rotations, [true; 3]);
        assert!(body.native().is_none(), "读回来不该带着原生句柄");

        let collider = restored[ball].collider().unwrap();
        assert_eq!(collider.desc().shape, ColliderShape::ball(0.75));
        assert_eq!(collider.desc().friction, 0.9);

        // 真正的验收：读回来的场景能直接跑物理。
        restored.step_physics(1.0 / 60.0);
        assert_eq!(restored.physics().body_count(), 2);
        assert_eq!(restored.physics().collider_count(), 2);
    }

    #[test]
    fn a_saved_scene_keeps_simulating_the_same_way() {
        // 「状态完全一致」最硬的检验：存档前后各跑一秒，落点必须一样。
        fn build() -> Scene {
            let mut scene = Scene::new();
            scene.add_node(
                Node::new("ground")
                    .with_position(Vec3::new(0.0, -0.5, 0.0))
                    .with_rigid_body(RigidBody::fixed())
                    .with_collider(Collider::cuboid(Vec3::new(20.0, 0.5, 20.0))),
            );
            scene.add_node(
                Node::new("ball")
                    .with_position(Vec3::new(0.3, 6.0, -0.2))
                    .with_rigid_body(RigidBody::dynamic())
                    .with_collider(Collider::ball(0.5)),
            );
            scene
        }

        fn settle(scene: &mut Scene) -> Vec3 {
            for _ in 0..180 {
                scene.step_physics(1.0 / 60.0);
                scene.update();
            }
            let ball = scene.find_by_name("ball").unwrap();
            scene[ball].transform.position
        }

        let mut original = build();
        let mut restored = roundtrip(&mut build());

        assert_eq!(settle(&mut restored), settle(&mut original));
    }

    #[test]
    fn joints_keep_pointing_at_the_right_nodes() {
        let mut scene = Scene::new();
        let anchor = scene.add_node(Node::new("anchor").with_rigid_body(RigidBody::fixed()));
        let bob = scene.add_node(
            Node::new("bob")
                .with_position(Vec3::X * 2.0)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.3)),
        );
        let joint = scene.add_node(Node::new("joint").with_joint(Joint::new(
            anchor,
            bob,
            JointDesc::revolute(Vec3::ZERO, Vec3::NEG_X * 2.0, Vec3::Z, Some([-1.0, 1.0])),
        )));

        let mut restored = roundtrip(&mut scene);

        let component = restored[joint].joint().unwrap();
        assert_eq!(component.body1(), anchor);
        assert_eq!(component.body2(), bob);
        assert!(matches!(component.desc().kind, JointKind::Revolute { .. }));

        restored.step_physics(1.0 / 60.0);
        assert_eq!(restored.physics().joint_count(), 1, "关节没被重建出来");
    }

    #[test]
    fn a_skin_survives_with_its_joints_and_inverse_binds() {
        let mut scene = Scene::new();
        let bone_a = scene.add_node(Node::new("bone_a"));
        let bone_b = scene.add_node(Node::new("bone_b").with_position(Vec3::Y));
        let node = scene.add_node(Node::new("skinned").with_mesh(Mesh::cube()).with_skin(
            Skin::new(
                vec![bone_a, bone_b],
                vec![Mat4::IDENTITY, Mat4::from_translation(Vec3::NEG_Y)],
            ),
        ));

        let restored = roundtrip(&mut scene);
        let skin = restored[node].skin().expect("骨架丢了");

        assert_eq!(skin.joints(), &[bone_a, bone_b]);
        assert_eq!(skin.inverse_bind()[1], Mat4::from_translation(Vec3::NEG_Y));
        // 骨骼矩阵不存盘，读回后由 `update` 算出来。
        assert_eq!(skin.matrices().len(), 2);
    }

    #[test]
    fn saving_the_same_scene_twice_produces_identical_bytes() {
        // 表的顺序由节点遍历序固定；不稳定的话「文件没变就不重写」无从谈起。
        let mut scene = Scene::new();
        for index in 0..20 {
            scene.add_node(
                Node::new(format!("n{index}"))
                    .with_mesh(Mesh::cube())
                    .with_material(PbrMaterial::dielectric(Vec3::splat(0.5), 0.5)),
            );
        }

        assert_eq!(scene.save_to_vec().unwrap(), scene.save_to_vec().unwrap());
    }

    #[test]
    fn a_future_format_version_is_rejected_instead_of_misread() {
        // 按老规则解释新文件，得到的是「读进来了但哪儿都不对」，比读不进来难查。
        let mut scene = Scene::new();
        let mut bytes = scene.save_to_vec().unwrap();

        // 把版本号那 4 个字节改成一个未来的版本。
        let needle = SCENE_FORMAT_VERSION.to_le_bytes();
        let position = bytes
            .windows(4)
            .position(|w| w == needle)
            .expect("文件里应当有版本号");
        bytes[position..position + 4].copy_from_slice(&999u32.to_le_bytes());

        assert!(Scene::load_from_slice(&bytes, None).is_err());
    }

    #[test]
    fn loading_without_a_manager_still_works_for_procedural_content() {
        // 程序化几何是内联的，不需要资源管理器也能读回来。
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("cube").with_mesh(Mesh::cube()));
        let bytes = scene.save_to_vec().unwrap();

        let restored = Scene::load_from_slice(&bytes, None).unwrap();

        assert!(restored[node].mesh().is_some());
    }

    #[test]
    fn an_unresolvable_mesh_reference_degrades_instead_of_failing_the_load() {
        // 模型文件被挪走了，不该让整个存档读不进来。
        let mut scene = Scene::new();
        let node =
            scene.add_node(Node::new("gone").with_mesh(
                Mesh::cube().with_source(kmesh::MeshSource::new("missing/nowhere.glb", 0)),
            ));
        let bytes = scene.save_to_vec().unwrap();

        let restored = Scene::load_from_slice(&bytes, Some(&manager())).expect("整体读档不该失败");

        assert!(restored[node].mesh().is_none());
        assert_eq!(restored[node].name, "gone", "节点本身要留着");
    }

    #[test]
    fn the_ascii_dump_is_written_and_readable() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("marker").with_position(Vec3::new(1.0, 2.0, 3.0)));

        let directory = std::env::temp_dir().join("kengine_scene_ascii_test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("scene.txt");
        scene.save_text(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("marker"), "存出来的文本里找不到节点名");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_scene_survives_a_trip_through_an_actual_file() {
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("disk")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(7.0, 8.0, 9.0)),
        );

        let directory = std::env::temp_dir().join("kengine_scene_file_test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("scene.bin");
        scene.save(&path).unwrap();

        let restored = Scene::load(&path, None).unwrap();

        assert_eq!(restored[node].transform.position, Vec3::new(7.0, 8.0, 9.0));
        assert!(restored[node].mesh().is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn probe_script_survives_save() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("n").with_script("spin.js"));
        let bytes = scene.save_to_vec().unwrap();
        let loaded = Scene::load_from_slice(&bytes, None).unwrap();
        let h = loaded.find_by_name("n").unwrap();
        println!("PROBE 脚本槽位 = {:?}", loaded.try_get(h).unwrap().script());
    }
}
