//! 实例化：一个节点、一份网格和材质，画 N 份。
//!
//! ```ignore
//! let trees: Vec<Instance> = positions.iter().map(|&p| Instance::at(p).with_color(tint)).collect();
//! scene.add_node(Node::new("Forest").with_mesh(tree).with_material(bark).with_instances(trees));
//! ```
//!
//! 和「N 个节点」比：场景这边只有一个节点（剔除、变换、包围盒都只算一次），渲染器那边只有一个绘制项
//! （材质解析、探针选择、600 字节的对象数据都只做一份），每个实例只多 112 字节（矩阵 + 颜色 + 一个
//! 自定义 `vec4`），一次 `draw_indexed` 画完。
//!
//! 代价：整组一起剔除（一组横跨整张地图的话，看见一棵就要画全部）——大组按区域切成几个节点；
//! 拾取、物理不认实例；实例矩阵变了没有运动向量（节点自己动是有的）。

use kmath::{Mat4, Quat, Vec3, Vec4};

use crate::Transform;

/// 一个实例。
///
/// 布局和着色器里的 `InstanceData` 逐字节一致（`repr(C)`、96 字节）：渲染器把一个节点的整组实例
/// 原样拷进显存，CPU 上不再逐个转换。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    /// 在节点里的局部变换：实例的世界变换 = 节点的世界变换 × 它。
    pub transform: Mat4,
    /// 乘到基础色上（rgba）。默认白色不透明。
    pub color: Vec4,
    /// 自定义数据，材质钩子里读 `vertex.instance_data` / `surface.instance_data`。
    pub data: Vec4,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            transform: Mat4::IDENTITY,
            color: Vec4::ONE,
            data: Vec4::ZERO,
        }
    }
}

impl Instance {
    /// 用一个局部矩阵建，颜色白、数据零。
    pub fn new(transform: Mat4) -> Self {
        Self {
            transform,
            ..Default::default()
        }
    }

    /// 放在节点里的某个位置。
    pub fn at(position: Vec3) -> Self {
        Self::new(Mat4::from_translation(position))
    }

    /// 位置、旋转、缩放。
    pub fn from_transform(transform: &Transform) -> Self {
        Self::new(transform.matrix())
    }

    /// 位置、旋转、缩放三样分开给。
    pub fn from_parts(position: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self::new(Mat4::from_scale_rotation_translation(
            scale, rotation, position,
        ))
    }

    /// 颜色（rgb，不透明）。
    pub fn with_color(mut self, color: Vec3) -> Self {
        self.color = color.extend(1.0);
        self
    }

    /// 自定义数据（钩子里的 `instance_data`）。
    pub fn with_data(mut self, data: Vec4) -> Self {
        self.data = data;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Node, Scene};
    use kmesh::Mesh;

    #[test]
    fn the_bounds_cover_every_instance_and_follow_changes() {
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("Row")
                .with_mesh(Mesh::cube())
                .with_position(Vec3::new(0.0, 0.0, 10.0))
                .with_instances(vec![
                    Instance::at(Vec3::new(-5.0, 0.0, 0.0)),
                    Instance::at(Vec3::new(5.0, 0.0, 0.0)),
                ]),
        );
        scene.update();
        let aabb = scene[node].global_aabb;
        assert!(
            (aabb.min.x + 5.5).abs() < 1e-4 && (aabb.max.x - 5.5).abs() < 1e-4,
            "{aabb:?}"
        );
        assert!((aabb.center().z - 10.0).abs() < 1e-4);
        // 改实例之后下一帧包围盒跟上。
        scene[node]
            .instances_mut()
            .unwrap()
            .push(Instance::at(Vec3::new(0.0, 8.0, 0.0)));
        scene.update();
        assert!((scene[node].global_aabb.max.y - 8.5).abs() < 1e-4);
        // 一个绘制项，带着三个实例。
        let items: Vec<_> = scene.visible_meshes().collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].instances.map(<[_]>::len), Some(3));
    }

    #[test]
    fn the_layout_matches_the_shader() {
        // geometry.wgsl 的 InstanceData：mat4x4 + vec4 + vec4。
        assert_eq!(std::mem::size_of::<Instance>(), 96);
        assert_eq!(std::mem::offset_of!(Instance, color), 64);
        assert_eq!(std::mem::offset_of!(Instance, data), 80);
    }
}
