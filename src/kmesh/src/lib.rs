//! kmesh —— 网格资源。
//!
//! 只描述 CPU 端的顶点与索引，**不依赖 wgpu**——顶点属性布局由渲染器声明，
//! 显存上传按 [`Mesh::id`] 缓存。这样网格能在没有图形设备的环境里加载与处理。
//!
//! ```
//! use kmesh::prelude::*;
//!
//! let cube = Mesh::cube();
//! assert_eq!(cube.vertices().len(), 24); // 6 面 × 4 顶点
//! assert_eq!(cube.index_count(), 36);
//! ```

#![warn(missing_docs)]

mod primitives;

use bytemuck::{Pod, Zeroable};
use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use kmath::{Aabb, Vec3};
use std::fmt;

/// [`Mesh`] 的资源类型标识。
pub const MESH_TYPE_UUID: Uuid = uuid!("5d7e9a32-1c48-4f60-8b93-a2e5c740d816");

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{Mesh, Vertex};
    pub use kmath::Aabb;
}

/// 单个顶点。
///
/// 字段顺序即着色器里的 `@location` 顺序，改动需同步渲染器的属性布局。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    /// 模型空间坐标。
    pub position: [f32; 3],
    /// 模型空间法线，用于光照。
    pub normal: [f32; 3],
    /// 纹理坐标。
    pub uv: [f32; 2],
    /// 顶点颜色，与材质基础色相乘。
    pub color: [f32; 3],
}

impl Default for Vertex {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
            color: [1.0; 3],
        }
    }
}

impl Vertex {
    /// 便捷构造，顶点色默认为白色。
    pub fn new(position: Vec3, normal: Vec3, uv: [f32; 2]) -> Self {
        Self {
            position: position.to_array(),
            normal: normal.to_array(),
            uv,
            color: [1.0; 3],
        }
    }

    /// 指定顶点色。
    pub fn with_color(mut self, color: Vec3) -> Self {
        self.color = color.to_array();
        self
    }

    /// 顶点位置。
    pub fn position(&self) -> Vec3 {
        Vec3::from_array(self.position)
    }

    /// 顶点法线。
    pub fn normal(&self) -> Vec3 {
        Vec3::from_array(self.normal)
    }
}

/// 一份网格数据。
///
/// 克隆共享同一个 `id`，渲染器据此避免重复上传显存——
/// 同一个网格用在多个节点上只占一份显存。
#[derive(Clone)]
pub struct Mesh {
    id: Uuid,
    vertices: Vec<Vertex>,
    /// 索引用 `u32`：真实模型经常超过 65535 个顶点。
    indices: Vec<u32>,
    /// 构造时算好的局部包围盒。剔除每帧都要用，不能每次重新遍历顶点。
    aabb: Aabb,
}

impl Mesh {
    /// 用自定义顶点与索引创建网格。
    pub fn new(vertices: Vec<Vertex>, indices: Vec<u32>) -> Self {
        let aabb = compute_aabb(&vertices);
        Self {
            id: Uuid::new_v4(),
            vertices,
            indices,
            aabb,
        }
    }

    /// 显存缓存键。克隆的网格共享同一个 id。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 顶点数据。
    pub fn vertices(&self) -> &[Vertex] {
        &self.vertices
    }

    /// 索引数据。
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// 索引数量，即 `draw_indexed` 的绘制量。
    pub fn index_count(&self) -> u32 {
        self.indices.len() as u32
    }

    /// 三角形数量。
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// 局部空间的轴对齐包围盒，构造时算好，查询是 O(1)。
    pub fn aabb(&self) -> Aabb {
        self.aabb
    }

    /// 根据三角形面法线重算顶点法线（相邻面取平均）。
    ///
    /// glTF 允许网格不带法线，这时必须自行生成，否则光照会全黑。
    pub fn recompute_normals(&mut self) {
        for vertex in &mut self.vertices {
            vertex.normal = [0.0; 3];
        }

        for triangle in self.indices.chunks_exact(3) {
            let [i0, i1, i2] = [
                triangle[0] as usize,
                triangle[1] as usize,
                triangle[2] as usize,
            ];
            if i0 >= self.vertices.len() || i1 >= self.vertices.len() || i2 >= self.vertices.len() {
                continue;
            }

            let p0 = self.vertices[i0].position();
            let p1 = self.vertices[i1].position();
            let p2 = self.vertices[i2].position();
            // 未归一化的面法线，其长度正比于三角形面积，
            // 这样大面片对顶点法线的贡献更大，结果更平滑。
            let face_normal = (p1 - p0).cross(p2 - p0);

            for index in [i0, i1, i2] {
                let current = self.vertices[index].normal();
                self.vertices[index].normal = (current + face_normal).to_array();
            }
        }

        for vertex in &mut self.vertices {
            // 退化三角形可能让法线长度为零，此时兜底朝上，避免出现 NaN。
            let normal = vertex.normal();
            vertex.normal = normal.try_normalize().unwrap_or(Vec3::Y).to_array();
        }
    }

    /// 把所有顶点乘上一个统一颜色。
    pub fn with_vertex_color(mut self, color: Vec3) -> Self {
        for vertex in &mut self.vertices {
            vertex.color = color.to_array();
        }
        self
    }

    /// 索引是否都在顶点范围内。
    pub fn is_valid(&self) -> bool {
        self.indices.len() % 3 == 0
            && self
                .indices
                .iter()
                .all(|i| (*i as usize) < self.vertices.len())
    }
}

fn compute_aabb(vertices: &[Vertex]) -> Aabb {
    let mut aabb = Aabb::EMPTY;
    for vertex in vertices {
        aabb.expand(vertex.position());
    }
    aabb
}

impl fmt::Debug for Mesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不打印顶点数组，一个模型可能有几十万个顶点。
        f.debug_struct("Mesh")
            .field("id", &self.id)
            .field("vertices", &self.vertices.len())
            .field("triangles", &self.triangle_count())
            .finish()
    }
}

impl ResourceData for Mesh {
    fn type_uuid(&self) -> Uuid {
        MESH_TYPE_UUID
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn clone_shares_gpu_id() {
        let mesh = Mesh::cube();
        assert_eq!(mesh.id(), mesh.clone().id());
        assert_ne!(mesh.id(), Mesh::cube().id());
    }

    #[test]
    fn aabb_covers_all_vertices() {
        let aabb = Mesh::cube().aabb();

        assert_eq!(aabb.min, Vec3::splat(-0.5));
        assert_eq!(aabb.max, Vec3::splat(0.5));
        assert_eq!(aabb.center(), Vec3::ZERO);
        assert_eq!(aabb.size(), Vec3::ONE);
    }


    #[test]
    fn recomputed_normals_match_flat_face() {
        // 一个位于 XZ 平面、朝上的三角形。
        let vertices = vec![
            Vertex::new(Vec3::new(0.0, 0.0, 0.0), Vec3::ZERO, [0.0, 0.0]),
            Vertex::new(Vec3::new(0.0, 0.0, 1.0), Vec3::ZERO, [0.0, 1.0]),
            Vertex::new(Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, [1.0, 0.0]),
        ];
        let mut mesh = Mesh::new(vertices, vec![0, 1, 2]);

        mesh.recompute_normals();

        for vertex in mesh.vertices() {
            assert!((vertex.normal() - Vec3::Y).length() < 1e-5);
        }
    }

    #[test]
    fn degenerate_triangle_does_not_produce_nan() {
        // 三个点共线，面法线为零向量。
        let vertices = vec![
            Vertex::new(Vec3::ZERO, Vec3::ZERO, [0.0; 2]),
            Vertex::new(Vec3::X, Vec3::ZERO, [0.0; 2]),
            Vertex::new(Vec3::X * 2.0, Vec3::ZERO, [0.0; 2]),
        ];
        let mut mesh = Mesh::new(vertices, vec![0, 1, 2]);

        mesh.recompute_normals();

        for vertex in mesh.vertices() {
            assert!(vertex.normal().is_finite(), "法线出现了 NaN 或 inf");
        }
    }

    #[test]
    fn out_of_range_indices_are_detected() {
        let mesh = Mesh::new(vec![Vertex::default()], vec![0, 1, 2]);

        assert!(!mesh.is_valid());
    }

    #[test]
    fn recompute_normals_ignores_out_of_range_indices() {
        // 越界索引不应导致 panic。
        let mut mesh = Mesh::new(vec![Vertex::default()], vec![0, 5, 9]);

        mesh.recompute_normals();

        assert_eq!(mesh.vertices().len(), 1);
    }

    #[test]
    fn cube_is_valid() {
        assert!(Mesh::cube().is_valid());
        assert!(Mesh::plane(1.0).is_valid());
        assert!(Mesh::sphere(8, 12).is_valid());
    }
}
