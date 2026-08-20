//! 地形与场景的接线。
//!
//! # 地形块是普通子节点
//!
//! 每块地形在场景里对应一个**挂着网格的普通节点**，父节点是地形节点。
//! 于是剔除、批处理、阴影、序列化全都白捡——渲染器根本不知道
//! 这些网格是地形生成的。
//!
//! 代价是块数多时节点也多（一块 1024² 切成 64 格一块是 256 个节点），
//! 但节点本身很轻，而 BVH 本来就要处理成千上万个物体。
//!
//! # 碰撞体是高度场，不是三角网格
//!
//! 三角网格碰撞体要把每个三角形都建进物理世界；高度场只存高度值，
//! 求交时按格子直接定位。一块 1024² 的地形，前者两百万个三角形，
//! 后者一百万个 `f32`——而且后者的射线查询快一个量级。

use crate::{Collider, Node, Scene};
use kcore::pool::Handle;
use kmath::Vec3;
use kphysics::{ColliderDesc, ColliderShape};

impl Scene {
    /// 按相机位置更新所有地形的 LOD，并重建变了的块。
    ///
    /// 由 [`Scene::update`] 自动调用，一般不必自己调。
    pub(crate) fn update_terrains(&mut self) {
        if self.index.terrains.is_empty() {
            return;
        }
        // 相机取的是**上一帧**的位置：地形排在世界变换重算之前，
        // 这时的索引还是上一帧建的。差一帧对 LOD 无所谓——
        // 它本来就是按几十上百米的距离分档的。
        //
        // 还没有相机时（第一帧）当作相机在原点，于是第一帧全用最细的
        // LOD。宁可多画一帧，也不要第一帧什么都不显示。
        let camera = self
            .active_camera()
            .map_or(Vec3::ZERO, |(to_world, _)| to_world.w_axis.truncate());

        for handle in self.index.terrains.clone() {
            let chunks = self.update_terrain(handle, camera);
            // 新建的块赶不上第一趟的树遍历，派生数据要在这里补齐：
            // 不补的话它们本帧不进剔除结构，相机一动远处的地形会
            // 闪一下才补上。
            self.adopt_terrain_chunks(handle, &chunks);
        }
    }

    /// 给刚生成的块补上世界变换、可见性，并塞进本帧的索引。
    fn adopt_terrain_chunks(&mut self, parent: Handle<Node>, chunks: &[Handle<Node>]) {
        let Some(node) = self.try_get(parent) else {
            return;
        };
        let (global, visible) = (node.global_transform, node.global_visible);

        for child in chunks {
            let Some(node) = self.try_get_mut(*child) else {
                continue;
            };
            // 块的局部变换是单位阵，世界变换直接继承父节点的。
            node.global_transform = global * node.transform.matrix();
            node.global_visible = visible && node.visible;
            let drawable = node.global_visible && node.mesh.is_some();
            if drawable && !self.index.drawables.contains(child) {
                self.index.drawables.push(*child);
            }
        }
    }

    /// 更新一块地形。
    fn update_terrain(&mut self, handle: Handle<Node>, camera: Vec3) -> Vec<Handle<Node>> {
        // 相机要换算到地形的**局部**坐标：地形被挪动或缩放之后，
        // 用世界坐标算距离会让 LOD 整体选错档。
        let to_local = self.world_matrix(handle).inverse();
        let local_camera = to_local.transform_point3(camera);

        // 地形本体要先**整个取出来**：生成网格时要读它，同时又要往场景里
        // 加子节点——两个可变借用碰在一起。取出来用完再放回去。
        let Some(mut terrain) = self.try_get_mut(handle).and_then(Node::take_terrain) else {
            return Vec::new();
        };

        let updates = terrain.update(local_camera);
        let mut touched = Vec::new();
        if !updates.is_empty() {
            let meshes: Vec<_> = updates
                .iter()
                .filter_map(|u| terrain.build_chunk(u.index).map(|m| (u.index, m)))
                .collect();
            touched = self.sync_terrain_chunks(handle, meshes);
        }

        if let Some(node) = self.try_get_mut(handle) {
            node.set_terrain(terrain);
        }
        touched
    }

    /// 把重建好的块网格写回子节点，必要时新建。
    fn sync_terrain_chunks(
        &mut self,
        handle: Handle<Node>,
        meshes: Vec<(usize, kmesh::Mesh)>,
    ) -> Vec<Handle<Node>> {
        // 块子节点按下标找。名字里带下标而不是靠顺序：
        // 靠顺序的话，中途插入一个别的子节点就会让所有块错位。
        let existing: Vec<(String, Handle<Node>)> = self
            .try_get(handle)
            .map(|node| {
                node.children()
                    .iter()
                    .filter_map(|child| self.try_get(*child).map(|c| (c.name.clone(), *child)))
                    .collect()
            })
            .unwrap_or_default();

        let material = self
            .try_get(handle)
            .and_then(|node| node.material().cloned());

        let mut touched = Vec::new();
        for (index, mesh) in meshes {
            let name = format!("__chunk{index}");
            match existing.iter().find(|(n, _)| *n == name) {
                Some((_, child)) => {
                    if let Some(node) = self.try_get_mut(*child) {
                        node.set_mesh(mesh);
                    }
                    touched.push(*child);
                }
                None => {
                    let mut node = Node::new(name).with_mesh(mesh);
                    if let Some(material) = material.clone() {
                        node.set_material(material);
                    }
                    let child = self.add_node_with_parent(node, handle);
                    touched.push(child);
                }
            }
        }
        touched
    }

    /// 给一块地形装上高度场碰撞体。
    ///
    /// 碰撞体挂在地形节点**自己**身上，不是块上——高度场本来就是一整张，
    /// 按块拆开只会在块与块的接缝处留下角色能卡进去的缝。
    pub fn attach_terrain_collider(&mut self, handle: Handle<Node>) {
        let Some(terrain) = self.try_get(handle).and_then(Node::terrain) else {
            return;
        };
        let (rows, cols, heights, scale) = terrain.collider_data();
        // rapier 的高度场以中心为原点，地形以角为原点，差半块。
        // 不补偏移，角色会踩在离视觉地面半个地形远的地方。
        let offset = terrain.collider_offset();

        let collider = Collider::new(
            ColliderDesc::new(ColliderShape::Heightfield {
                rows,
                cols,
                heights: std::sync::Arc::new(heights),
                scale,
            })
            .with_offset(offset, kmath::Quat::IDENTITY),
        );
        if let Some(node) = self.try_get_mut(handle) {
            node.set_collider(collider);
        }
    }

    /// 一条世界空间的射线打在哪块地形上。
    ///
    /// 返回 `(地形节点, 世界空间的命中点)`。笔刷落点靠它。
    pub fn raycast_terrain(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<(Handle<Node>, Vec3)> {
        let mut best: Option<(Handle<Node>, Vec3, f32)> = None;

        for handle in &self.index.terrains {
            let Some(terrain) = self.try_get(*handle).and_then(Node::terrain) else {
                continue;
            };
            let to_world = self.world_matrix(*handle);
            let to_local = to_world.inverse();

            let local_origin = to_local.transform_point3(origin);
            // 方向只做旋转不做平移，用 `transform_vector3`。
            // 用 `transform_point3` 的话方向会被平移带偏，射线整个歪掉。
            let local_direction = to_local.transform_vector3(direction);

            let Some(hit) = terrain.raycast(local_origin, local_direction, max_distance) else {
                continue;
            };
            let world = to_world.transform_point3(hit);
            let distance = (world - origin).length();
            // 多块地形重叠时取最近的那个。
            if best.is_none_or(|(_, _, d)| distance < d) {
                best = Some((*handle, world, distance));
            }
        }

        best.map(|(handle, point, _)| (handle, point))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Camera, Transform};
    use kmath::{Vec2, Vec3};
    use kterrain::{Heightmap, Terrain};

    fn terrain() -> Terrain {
        let mut map = Heightmap::flat(17, 17, Vec2::new(160.0, 160.0));
        for row in 0..map.rows() {
            for col in 0..map.cols() {
                map.set_height(col, row, (col as f32 * 0.5).sin() * 4.0);
            }
        }
        Terrain::new(map, 8, 2)
    }

    /// 一个带地形和相机的场景。
    fn scene() -> (Scene, Handle<Node>) {
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::from_position(Vec3::new(80.0, 50.0, 80.0))),
        );
        let handle = scene.add_node(Node::new("Terrain").with_terrain(terrain()));
        scene.update();
        (scene, handle)
    }

    #[test]
    fn chunks_become_child_nodes_with_meshes() {
        let (scene, handle) = scene();
        let children = scene.try_get(handle).unwrap().children().to_vec();
        assert_eq!(children.len(), 4, "16×16 格切成 8 格一块该是四块");
        for child in children {
            assert!(
                scene.try_get(child).unwrap().mesh().is_some(),
                "块子节点该带网格"
            );
        }
    }

    #[test]
    fn chunks_are_not_duplicated_on_later_frames() {
        // 每帧新建一批的话，节点数会一帧帧涨，几秒钟就把池撑爆。
        let (mut scene, handle) = scene();
        for _ in 0..10 {
            scene.update();
        }
        assert_eq!(scene.try_get(handle).unwrap().children().len(), 4);
    }

    #[test]
    fn chunk_meshes_are_rebuilt_when_the_camera_moves() {
        let (mut scene, handle) = scene();
        let child = scene.try_get(handle).unwrap().children()[3];
        let before = scene
            .try_get(child)
            .unwrap()
            .mesh()
            .unwrap()
            .vertices()
            .len();

        // 相机跑远，远处的块该降到更粗的 LOD。
        let camera = scene.find_by_name("Camera").unwrap();
        scene[camera].transform.position = Vec3::new(2000.0, 500.0, 2000.0);
        scene.update();

        let after = scene
            .try_get(child)
            .unwrap()
            .mesh()
            .unwrap()
            .vertices()
            .len();
        assert!(after < before, "远处的块该变粗：{before} → {after}");
    }

    #[test]
    fn terrain_chunks_enter_the_culling_structure() {
        // 块是普通子节点，所以剔除、批处理、阴影全都白捡。
        let (scene, _) = scene();
        assert!(scene.drawable_count() >= 4);
    }

    #[test]
    fn the_lod_follows_the_terrain_local_position() {
        // 地形被挪走之后，用世界坐标算距离会让 LOD 整体选错档。
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::from_position(Vec3::new(5000.0, 50.0, 5000.0))),
        );
        // 地形挪到相机脚下：局部坐标里相机就在地形上方，该用最细的 LOD。
        let handle = scene.add_node(
            Node::new("Terrain")
                .with_terrain(terrain())
                .with_position(Vec3::new(5000.0, 0.0, 5000.0)),
        );
        scene.update();

        let child = scene.try_get(handle).unwrap().children()[0];
        let vertices = scene
            .try_get(child)
            .unwrap()
            .mesh()
            .unwrap()
            .vertices()
            .len();
        assert_eq!(vertices, 9 * 9, "相机就在这块上方，该是 LOD 0");
    }

    #[test]
    fn a_heightfield_collider_can_be_attached() {
        let (mut scene, handle) = scene();
        scene.attach_terrain_collider(handle);
        assert!(scene.try_get(handle).unwrap().collider().is_some());

        scene.update();
        scene.step_physics(1.0 / 60.0);
        assert!(scene.physics().collider_count() > 0);
    }

    #[test]
    fn the_collider_lines_up_with_the_visual_surface() {
        // rapier 的高度场以中心为原点，地形以角为原点。不补偏移的话，
        // 角色会踩在离视觉地面半个地形远的地方。
        let (mut scene, handle) = scene();
        scene.attach_terrain_collider(handle);
        scene.update();
        scene.step_physics(1.0 / 60.0);

        // 在地形正中往下打一条物理射线，命中高度该和高度图对得上。
        let center = Vec3::new(80.0, 100.0, 80.0);
        let hit = scene
            .cast_ray(&kphysics::RayCastOptions::new(center, Vec3::NEG_Y, 200.0))
            .expect("物理射线该打中地形");

        let expected = scene
            .try_get(handle)
            .unwrap()
            .terrain()
            .unwrap()
            .heightmap()
            .sample(80.0, 80.0);
        assert!(
            (hit.point.y - expected).abs() < 1.0,
            "物理地面在 {}，视觉地面在 {expected}",
            hit.point.y
        );
    }

    #[test]
    fn raycasting_finds_the_surface_in_world_space() {
        let (scene, handle) = scene();
        let (hit_handle, point) = scene
            .raycast_terrain(Vec3::new(80.0, 100.0, 80.0), Vec3::NEG_Y, 300.0)
            .expect("垂直往下该打中地形");

        assert_eq!(hit_handle, handle);
        let expected = scene
            .try_get(handle)
            .unwrap()
            .terrain()
            .unwrap()
            .heightmap()
            .sample(80.0, 80.0);
        assert!((point.y - expected).abs() < 0.1);
    }

    #[test]
    fn raycasting_respects_the_terrain_transform() {
        // 方向被平移带偏的话射线整个歪掉，命中点会落在别处。
        let mut scene = Scene::new();
        scene.add_node(Node::new("Camera").with_camera(Camera::default()));
        scene.add_node(
            Node::new("Terrain")
                .with_terrain(terrain())
                .with_position(Vec3::new(1000.0, 20.0, -500.0)),
        );
        scene.update();

        let origin = Vec3::new(1080.0, 200.0, -420.0);
        let (_, point) = scene
            .raycast_terrain(origin, Vec3::NEG_Y, 500.0)
            .expect("挪走之后照样该打中");

        assert!((point.x - origin.x).abs() < 0.1, "命中点的 XZ 不该偏");
        assert!((point.z - origin.z).abs() < 0.1);
        // 地形被抬高了 20，命中高度也该跟着抬。
        assert!(point.y > 15.0, "命中高度 {} 没跟着地形抬起来", point.y);
    }

    #[test]
    fn a_ray_that_misses_returns_nothing() {
        let (scene, _) = scene();
        assert!(
            scene
                .raycast_terrain(Vec3::new(80.0, 100.0, 80.0), Vec3::Y, 300.0)
                .is_none()
        );
    }
}
