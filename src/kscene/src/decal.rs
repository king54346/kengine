//! 把贴花投影到场景几何上。
//!
//! 几何裁剪本身在 [`kmesh::decal`]，这里负责的是**找接收面**：
//! 遍历场景里和贴花体相交的可绘制节点，把它们的网格裁一遍，
//! 合成一个贴花节点。

use crate::{Node, Scene};
use kcore::pool::Handle;
use kmaterial::{BlendMode, Material};
use kmesh::{
    Mesh,
    decal::{Decal, project},
};

/// 一次贴花投影的设置。
#[derive(Debug, Clone)]
pub struct DecalOptions {
    /// 投影体。
    pub decal: Decal,
    /// 贴花用的材质。
    ///
    /// 会被强制设成 [`BlendMode::Alpha`]——贴花图边缘必然是透明的，
    /// 不混合的话弹孔会变成一个不透明的方块。
    pub material: Material,
    /// 节点名字，方便之后按名字找出来删掉。
    pub name: String,
    /// 只投影到这棵子树上；[`None`] 表示整个场景。
    ///
    /// 用来避免血迹溅到 UI 用的辅助几何上，或者只贴在某个建筑上。
    pub root: Option<Handle<Node>>,
}

impl DecalOptions {
    /// 用给定的投影体和材质构造。
    pub fn new(decal: Decal, material: Material) -> Self {
        Self {
            decal,
            material,
            name: "Decal".to_string(),
            root: None,
        }
    }

    /// 设置节点名。
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// 限定投影范围。
    pub fn with_root(mut self, root: Handle<Node>) -> Self {
        self.root = Some(root);
        self
    }
}

impl Scene {
    /// 把一块贴花投影到场景几何上，生成一个贴花节点。
    ///
    /// 一个接收面都没找到时返回 [`None`]——比如往天上开了一枪。
    ///
    /// # 前置条件
    ///
    /// 依赖节点的世界变换和包围盒，所以**必须在 [`Scene::update`] 之后调用**。
    /// 刚加进场景还没 update 过的节点，世界变换还是单位阵，贴花会落在错的地方。
    ///
    /// # 只对静态几何有效
    ///
    /// 生成的是一片固定的网格。接收物之后再动，贴花不会跟着动——
    /// 它会留在原地。蒙皮节点直接跳过（见 [`kmesh::decal`] 里的说明）。
    pub fn spawn_decal(&mut self, options: &DecalOptions) -> Option<Handle<Node>> {
        let bounds = options.decal.bounds();

        // 先按包围盒粗筛。不筛的话每放一个弹孔都要把整个场景的
        // 三角形裁一遍——一场枪战下来就卡死了。
        let receivers: Vec<Handle<Node>> = self
            .index
            .drawables
            .iter()
            .copied()
            .filter(|handle| {
                let Some(node) = self.try_get(*handle) else {
                    return false;
                };
                // 蒙皮几何每帧都在变，贴上去的网格下一帧就对不上了。
                if node.skin.is_some() {
                    return false;
                }
                if !node.receives_decals {
                    return false;
                }
                if !node.global_visible || !node.global_aabb.intersects(&bounds) {
                    return false;
                }
                match options.root {
                    Some(root) => self.is_descendant_of(*handle, root),
                    None => true,
                }
            })
            .collect();

        // 所有接收面合成一个网格：一次弹孔打在墙和地板的交界处会命中
        // 两个物体，分成两个节点的话渲染器要多一次绘制调用，
        // 而且删的时候要记住删两个。
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for handle in receivers {
            let node = &self.nodes[handle];
            let Some(mesh) = node.mesh() else { continue };
            let Some(piece) = project(mesh, node.global_transform, &options.decal) else {
                continue;
            };

            let base = vertices.len() as u32;
            vertices.extend_from_slice(piece.vertices());
            indices.extend(piece.indices().iter().map(|index| index + base));
        }

        if indices.is_empty() {
            return None;
        }

        // 贴花网格已经在世界空间里了，所以节点的变换保持单位阵。
        let mut material = options.material.clone();
        material.set_blend_mode(BlendMode::Alpha);

        let mut node = Node::new(options.name.clone())
            .with_mesh(Mesh::new(vertices, indices))
            .with_material(material);
        node.receives_decals = false;
        Some(self.add_node(node))
    }

    /// `handle` 是否在以 `root` 为根的子树里（含 `root` 自己）。
    fn is_descendant_of(&self, handle: Handle<Node>, root: Handle<Node>) -> bool {
        let mut current = handle;
        loop {
            if current == root {
                return true;
            }
            match self.try_get(current) {
                Some(node) if node.parent.is_some() => current = node.parent,
                _ => return false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kmath::{Mat4, Vec3};
    use kmesh::Vertex;

    fn floor(size: f32) -> Mesh {
        let h = size * 0.5;
        Mesh::new(
            vec![
                Vertex::new(Vec3::new(-h, 0.0, -h), Vec3::Y, [0.0, 0.0]),
                Vertex::new(Vec3::new(h, 0.0, -h), Vec3::Y, [1.0, 0.0]),
                Vertex::new(Vec3::new(h, 0.0, h), Vec3::Y, [1.0, 1.0]),
                Vertex::new(Vec3::new(-h, 0.0, h), Vec3::Y, [0.0, 1.0]),
            ],
            vec![0, 2, 1, 0, 3, 2],
        )
    }

    /// 一个装了地板的场景，已经 update 过。
    fn scene_with_floor() -> (Scene, Handle<Node>) {
        let mut scene = Scene::new();
        let handle = scene.add_node(Node::new("Floor").with_mesh(floor(20.0)));
        scene.update();
        (scene, handle)
    }

    fn options_at(position: Vec3) -> DecalOptions {
        DecalOptions::new(
            Decal::new(position, Vec3::Y, Vec3::splat(1.0), 0.0),
            Material::standard(),
        )
    }

    #[test]
    fn a_decal_lands_on_the_floor() {
        let (mut scene, _) = scene_with_floor();
        let handle = scene.spawn_decal(&options_at(Vec3::ZERO)).expect("该命中");
        assert!(scene.try_get(handle).and_then(Node::mesh).is_some());
    }

    #[test]
    fn a_decal_in_thin_air_produces_nothing() {
        // 往天上开一枪不该在场景里留下一个空节点。
        let (mut scene, _) = scene_with_floor();
        let before = scene.nodes().alive_count();
        assert!(
            scene
                .spawn_decal(&options_at(Vec3::new(0.0, 50.0, 0.0)))
                .is_none()
        );
        assert_eq!(scene.nodes().alive_count(), before);
    }

    #[test]
    fn the_decal_material_is_forced_to_alpha() {
        // 贴花图边缘必然是透明的，不混合的话弹孔会变成一个不透明的方块。
        let (mut scene, _) = scene_with_floor();
        let options = DecalOptions::new(
            Decal::new(Vec3::ZERO, Vec3::Y, Vec3::splat(1.0), 0.0),
            // 故意传一个不透明材质。
            Material::standard(),
        );
        assert_eq!(options.material.blend_mode(), BlendMode::Opaque);

        let handle = scene.spawn_decal(&options).unwrap();
        assert_eq!(
            scene
                .try_get(handle)
                .unwrap()
                .material()
                .unwrap()
                .blend_mode(),
            BlendMode::Alpha
        );
    }

    #[test]
    fn the_receiver_world_transform_is_used() {
        // 依赖 `update` 算出的世界变换。地板被挪到 x=50 之后，
        // 原点上的贴花不该再命中它。
        let mut scene = Scene::new();
        let floor_handle = scene.add_node(Node::new("Floor").with_mesh(floor(20.0)));
        scene.try_get_mut(floor_handle).unwrap().transform.position = Vec3::new(50.0, 0.0, 0.0);
        scene.update();

        assert!(scene.spawn_decal(&options_at(Vec3::ZERO)).is_none());
        assert!(
            scene
                .spawn_decal(&options_at(Vec3::new(50.0, 0.0, 0.0)))
                .is_some()
        );
    }

    #[test]
    fn several_receivers_merge_into_one_node() {
        // 打在墙和地板交界处会命中两个物体。分成两个节点的话渲染器
        // 要多一次绘制调用，删的时候还要记住删两个。
        let mut scene = Scene::new();
        for x in [-0.4f32, 0.4] {
            let handle = scene.add_node(Node::new("Floor").with_mesh(floor(0.6)));
            scene.try_get_mut(handle).unwrap().transform.position = Vec3::new(x, 0.0, 0.0);
        }
        scene.update();

        let before = scene.nodes().alive_count();
        let handle = scene
            .spawn_decal(&DecalOptions::new(
                Decal::new(Vec3::ZERO, Vec3::Y, Vec3::splat(4.0), 0.0),
                Material::standard(),
            ))
            .unwrap();
        assert_eq!(scene.nodes().alive_count(), before + 1, "该只加一个节点");

        // 两块地板各两个三角形，都完整落在贴花体内。
        // 裁剪不共享顶点，所以是 2 块 × 2 三角形 × 3 索引。
        assert_eq!(
            scene
                .try_get(handle)
                .unwrap()
                .mesh()
                .unwrap()
                .indices()
                .len(),
            12
        );
    }

    #[test]
    fn merged_indices_stay_in_range() {
        // 合并多个接收面时索引要整体偏移。忘了偏移的话第二块的
        // 三角形会引用第一块的顶点——贴花变成一团乱线。
        let mut scene = Scene::new();
        for x in [-0.4f32, 0.4] {
            let handle = scene.add_node(Node::new("Floor").with_mesh(floor(0.6)));
            scene.try_get_mut(handle).unwrap().transform.position = Vec3::new(x, 0.0, 0.0);
        }
        scene.update();

        let handle = scene
            .spawn_decal(&DecalOptions::new(
                Decal::new(Vec3::ZERO, Vec3::Y, Vec3::splat(4.0), 0.0),
                Material::standard(),
            ))
            .unwrap();
        let mesh = scene.try_get(handle).unwrap().mesh().unwrap();
        let count = mesh.vertices().len() as u32;
        assert!(mesh.indices().iter().all(|index| *index < count));
        // 两块地板的顶点都在，不是只有一块。裁剪逐三角形生成顶点、
        // 不做共享，所以是 2 块 × 2 三角形 × 3 顶点。
        assert_eq!(count, 12);
    }

    #[test]
    fn invisible_receivers_are_skipped() {
        let (mut scene, floor_handle) = scene_with_floor();
        scene.try_get_mut(floor_handle).unwrap().visible = false;
        scene.update();
        assert!(scene.spawn_decal(&options_at(Vec3::ZERO)).is_none());
    }

    #[test]
    fn the_root_filter_limits_the_receivers() {
        // 血迹不该溅到辅助几何上。
        let mut scene = Scene::new();
        let group = scene.add_node(Node::new("Group"));
        let inside = scene.add_node_with_parent(Node::new("Inside").with_mesh(floor(20.0)), group);
        let _outside = scene.add_node(Node::new("Outside").with_mesh(floor(20.0)));
        scene.update();

        // 限定在 group 里时只命中 inside。
        let handle = scene
            .spawn_decal(&options_at(Vec3::ZERO).with_root(group))
            .unwrap();
        let limited = scene
            .try_get(handle)
            .unwrap()
            .mesh()
            .unwrap()
            .indices()
            .len();

        // 不限定时两块都命中，几何量正好翻倍。
        let all = scene.spawn_decal(&options_at(Vec3::ZERO)).unwrap();
        let unlimited = scene.try_get(all).unwrap().mesh().unwrap().indices().len();

        assert_eq!(unlimited, limited * 2, "限定范围没起作用");
        assert!(scene.try_get(inside).is_some());
    }

    #[test]
    fn the_root_filter_accepts_the_root_itself() {
        let (mut scene, floor_handle) = scene_with_floor();
        assert!(
            scene
                .spawn_decal(&options_at(Vec3::ZERO).with_root(floor_handle))
                .is_some()
        );
    }

    #[test]
    fn a_decal_does_not_receive_another_decal() {
        // 贴花节点自己也是可绘制的。第二发子弹打在同一处时，
        // 如果把第一个贴花当接收面，贴花会一层层叠上去，
        // 每一层都比上一层高 offset——很快就浮在半空了。
        let (mut scene, _) = scene_with_floor();
        let first = scene.spawn_decal(&options_at(Vec3::ZERO)).unwrap();
        let baseline = scene
            .try_get(first)
            .unwrap()
            .mesh()
            .unwrap()
            .indices()
            .len();
        scene.update();

        // 第二个只该命中地板，几何量和第一个一样。
        let second = scene.spawn_decal(&options_at(Vec3::ZERO)).unwrap();
        assert_eq!(
            scene
                .try_get(second)
                .unwrap()
                .mesh()
                .unwrap()
                .indices()
                .len(),
            baseline,
            "第二个贴花贴到第一个上面去了"
        );

        // 反证：把开关打开，第一个贴花就会变成接收面，几何量翻倍。
        // 没有这一步的话，上面那条断言在「贴花根本没被当成可绘制节点」
        // 的情况下也会通过，测不出开关有没有生效。
        scene.try_get_mut(first).unwrap().receives_decals = true;
        scene.update();
        let third = scene.spawn_decal(&options_at(Vec3::ZERO)).unwrap();
        assert!(
            scene
                .try_get(third)
                .unwrap()
                .mesh()
                .unwrap()
                .indices()
                .len()
                > baseline,
            "开关没起作用：贴花本来就没被当成接收面"
        );
    }

    #[test]
    fn the_node_name_is_used() {
        let (mut scene, _) = scene_with_floor();
        let handle = scene
            .spawn_decal(&options_at(Vec3::ZERO).with_name("BulletHole"))
            .unwrap();
        assert_eq!(scene.try_get(handle).unwrap().name, "BulletHole");
        assert_eq!(scene.find_by_name("BulletHole"), Some(handle));
    }

    #[test]
    fn removing_a_decal_leaves_the_scene_intact() {
        let (mut scene, floor_handle) = scene_with_floor();
        let handle = scene.spawn_decal(&options_at(Vec3::ZERO)).unwrap();
        scene.remove_node(handle);
        scene.update();

        assert!(scene.try_get(handle).is_none());
        assert!(scene.try_get(floor_handle).is_some());
        // 删掉之后还能再贴。
        assert!(scene.spawn_decal(&options_at(Vec3::ZERO)).is_some());
    }

    #[test]
    fn the_decal_geometry_is_in_world_space() {
        // 节点变换保持单位阵，所以网格顶点必须已经是世界坐标。
        let mut scene = Scene::new();
        let floor_handle = scene.add_node(Node::new("Floor").with_mesh(floor(20.0)));
        scene.try_get_mut(floor_handle).unwrap().transform.position = Vec3::new(7.0, 0.0, 0.0);
        scene.update();

        let handle = scene
            .spawn_decal(&options_at(Vec3::new(7.0, 0.0, 0.0)))
            .unwrap();
        let node = scene.try_get(handle).unwrap();
        assert_eq!(node.transform.position, Vec3::ZERO);
        assert_eq!(node.global_transform, Mat4::IDENTITY);

        for vertex in node.mesh().unwrap().vertices() {
            assert!(
                (vertex.position[0] - 7.0).abs() <= 0.51,
                "顶点该在 x=7 附近，实测 {}",
                vertex.position[0]
            );
        }
    }
}
