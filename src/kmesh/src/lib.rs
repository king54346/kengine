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
    pub use crate::{Mesh, MorphDelta, MorphTarget, SkinVertex, Vertex};
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
    /// 切线，xyz 为方向、w 为副切线的手性（±1）。
    ///
    /// 法线贴图需要它来构建切线空间；没有法线贴图时该字段不参与计算。
    pub tangent: [f32; 4],
}

impl Default for Vertex {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
            color: [1.0; 3],
            tangent: [1.0, 0.0, 0.0, 1.0],
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
            tangent: [1.0, 0.0, 0.0, 1.0],
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

    /// 切线方向（不含手性）。
    pub fn tangent(&self) -> Vec3 {
        Vec3::new(self.tangent[0], self.tangent[1], self.tangent[2])
    }
}

/// 顶点的蒙皮属性。
///
/// 单独一个结构而不是塞进 [`Vertex`]：静态网格占绝大多数，
/// 让它们每个顶点白背 24 字节不划算。渲染器把它作为**第二个顶点缓冲**绑定，
/// 只有蒙皮管线才会读它。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct SkinVertex {
    /// 影响本顶点的四个关节，值是关节在 skin 关节表里的序号。
    ///
    /// 用 `u16` 而不是 `u32`：GPU 有原生的 `Uint16x4` 顶点格式，
    /// 而 6 万多个关节的骨架并不存在。
    pub joints: [u16; 4],
    /// 四个关节各自的影响权重，和为 1。
    pub weights: [f32; 4],
}

impl Default for SkinVertex {
    fn default() -> Self {
        // 全权重给 0 号关节：等价于「刚性绑定在根骨上」，是个安全的兜底。
        Self {
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
        }
    }
}

impl SkinVertex {
    /// 权重之和。
    pub fn weight_sum(&self) -> f32 {
        self.weights.iter().sum()
    }

    /// 把权重归一化到和为 1。
    ///
    /// glTF 规范要求权重和为 1，但导出器不总是照做；不归一化的话，
    /// 权重和偏大的顶点会被整体拉离骨骼。
    pub fn normalize(&mut self) {
        let sum = self.weight_sum();
        if sum > f32::EPSILON {
            for weight in &mut self.weights {
                *weight /= sum;
            }
        } else {
            // 一个权重都没有：退回刚性绑定，而不是让顶点塌到原点。
            *self = Self::default();
        }
    }
}

/// 一个顶点在某个形变目标下的增量。
///
/// 布局直接给 GPU 用，所以两个 `vec3` 各自补齐到 16 字节——
/// WGSL 里 `vec3` 的对齐要求就是 16，不补的话两边的字段会错位。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct MorphDelta {
    /// 位置增量。
    pub position: [f32; 3],
    /// 对齐填充。
    pub padding0: f32,
    /// 法线增量。形变后法线也得跟着变，否则光照会露馅。
    pub normal: [f32; 3],
    /// 对齐填充。
    pub padding1: f32,
}

/// 一个形变目标（blend shape）：一整套逐顶点的增量。
///
/// 最终顶点 = 基础顶点 + Σ 权重ᵢ × 增量ᵢ。权重通常在 `[0, 1]`，
/// 但规范并不限制——超出这个范围会得到夸张的外插效果，有时正是想要的。
#[derive(Clone, Debug, PartialEq)]
pub struct MorphTarget {
    name: String,
    deltas: Vec<MorphDelta>,
    /// 位置增量的范围，构造时算好。
    ///
    /// 剔除要用：形变会把顶点推出绑定姿态的包围盒，逐顶点重算太贵，
    /// 按「权重 × 增量范围」放大是个便宜且保守的近似。
    delta_bounds: Aabb,
}

impl MorphTarget {
    /// 用逐顶点增量构造。
    pub fn new(name: impl Into<String>, deltas: Vec<MorphDelta>) -> Self {
        let mut delta_bounds = Aabb::EMPTY;
        for delta in &deltas {
            delta_bounds.expand(Vec3::from_array(delta.position));
        }

        Self {
            name: name.into(),
            deltas,
            delta_bounds,
        }
    }

    /// 位置增量的范围。权重为 1 时顶点最多被推到这个范围的边界。
    pub fn delta_bounds(&self) -> Aabb {
        self.delta_bounds
    }

    /// 目标名，来自 glTF 的 `targetNames`；没有时是 `Target0` 这样的占位名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 逐顶点增量，与网格顶点一一对应。
    pub fn deltas(&self) -> &[MorphDelta] {
        &self.deltas
    }

    /// 顶点数量。
    pub fn len(&self) -> usize {
        self.deltas.len()
    }

    /// 是否没有任何增量。
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
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
    /// 蒙皮属性，与 `vertices` 一一对应。静态网格为 [`None`]。
    skin: Option<Vec<SkinVertex>>,
    /// 形变目标。每个目标都与 `vertices` 一一对应。
    morph_targets: Vec<MorphTarget>,
    /// 形变权重的初始值，与 `morph_targets` 一一对应。
    morph_weights: Vec<f32>,
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
            skin: None,
            morph_targets: Vec::new(),
            morph_weights: Vec::new(),
            aabb,
        }
    }

    /// 附上蒙皮属性，使该网格可以被骨骼驱动。
    ///
    /// 数量与顶点数对不上时忽略并告警——宁可退化成静态网格，
    /// 也不要在渲染时按错位的下标去取关节。
    pub fn with_skin(mut self, mut skin: Vec<SkinVertex>) -> Self {
        if skin.len() != self.vertices.len() {
            return self;
        }
        for vertex in &mut skin {
            vertex.normalize();
        }
        self.skin = Some(skin);
        self
    }

    /// 蒙皮属性；静态网格返回 [`None`]。
    pub fn skin(&self) -> Option<&[SkinVertex]> {
        self.skin.as_deref()
    }

    /// 是否是蒙皮网格。
    pub fn is_skinned(&self) -> bool {
        self.skin.is_some()
    }

    /// 附上形变目标与它们的初始权重。
    ///
    /// 增量数与顶点数对不上的目标会被丢掉：错位的增量会把网格撕开，
    /// 少一个形变总比整个模型炸掉强。权重数量不足时补 0。
    pub fn with_morph_targets(
        mut self,
        targets: Vec<MorphTarget>,
        mut weights: Vec<f32>,
    ) -> Self {
        let vertex_count = self.vertices.len();
        let targets: Vec<MorphTarget> = targets
            .into_iter()
            .filter(|target| target.len() == vertex_count)
            .collect();

        weights.resize(targets.len(), 0.0);
        self.morph_targets = targets;
        self.morph_weights = weights;
        self
    }

    /// 全部形变目标。
    pub fn morph_targets(&self) -> &[MorphTarget] {
        &self.morph_targets
    }

    /// 形变目标数量。
    pub fn morph_target_count(&self) -> usize {
        self.morph_targets.len()
    }

    /// 是否带形变目标。
    pub fn has_morph_targets(&self) -> bool {
        !self.morph_targets.is_empty()
    }

    /// 形变权重的初始值。实例化到场景时以它为起点。
    pub fn morph_weights(&self) -> &[f32] {
        &self.morph_weights
    }

    /// 按名字找形变目标的序号。
    pub fn find_morph_target(&self, name: &str) -> Option<usize> {
        self.morph_targets
            .iter()
            .position(|target| target.name() == name)
    }

    /// 按给定权重算出形变后的局部包围盒。
    ///
    /// 逐顶点重算太贵（每帧、每个形变网格都要走一遍顶点），
    /// 这里按「权重 × 该目标的增量范围」把基础包围盒撑开——
    /// 结果偏保守（可能比实际大），但剔除只怕小不怕大。
    pub fn morphed_aabb(&self, weights: &[f32]) -> Aabb {
        if self.morph_targets.is_empty() {
            return self.aabb;
        }

        let mut bounds = self.aabb;
        for (target, weight) in self.morph_targets.iter().zip(weights) {
            if *weight == 0.0 || target.delta_bounds.is_empty() {
                continue;
            }
            // 权重可以是负的，两个方向都要放开。
            let extent = target.delta_bounds.min.abs().max(target.delta_bounds.max.abs())
                * weight.abs();
            bounds = Aabb::new(bounds.min - extent, bounds.max + extent);
        }
        bounds
    }

    /// 按给定权重算出形变后的某个顶点位置。
    ///
    /// GPU 上做的是同一件事，这里供 CPU 侧（包围盒、拾取、测试）使用。
    pub fn morphed_position(&self, vertex: usize, weights: &[f32]) -> Vec3 {
        let mut position = self
            .vertices
            .get(vertex)
            .map(Vertex::position)
            .unwrap_or(Vec3::ZERO);

        for (target, weight) in self.morph_targets.iter().zip(weights) {
            if *weight == 0.0 {
                continue;
            }
            if let Some(delta) = target.deltas.get(vertex) {
                position += Vec3::from_array(delta.position) * *weight;
            }
        }
        position
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

    /// 根据 UV 与位置生成切线。
    ///
    /// glTF 允许网格不带 TANGENT，而法线贴图必须有切线空间才能工作。
    /// 算法：逐三角形用 UV 差分求切线，累加到顶点后做 Gram-Schmidt 正交化。
    pub fn recompute_tangents(&mut self) {
        let mut accumulated = vec![Vec3::ZERO; self.vertices.len()];
        let mut bitangents = vec![Vec3::ZERO; self.vertices.len()];

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
            let edge1 = self.vertices[i1].position() - p0;
            let edge2 = self.vertices[i2].position() - p0;

            let uv0 = self.vertices[i0].uv;
            let duv1 = [
                self.vertices[i1].uv[0] - uv0[0],
                self.vertices[i1].uv[1] - uv0[1],
            ];
            let duv2 = [
                self.vertices[i2].uv[0] - uv0[0],
                self.vertices[i2].uv[1] - uv0[1],
            ];

            // UV 面积为零时无法定出切线方向（例如所有 UV 相同），跳过该三角形。
            let determinant = duv1[0] * duv2[1] - duv2[0] * duv1[1];
            if determinant.abs() < 1e-12 {
                continue;
            }
            let r = 1.0 / determinant;

            let tangent = (edge1 * duv2[1] - edge2 * duv1[1]) * r;
            let bitangent = (edge2 * duv1[0] - edge1 * duv2[0]) * r;

            for index in [i0, i1, i2] {
                accumulated[index] += tangent;
                bitangents[index] += bitangent;
            }
        }

        for (index, vertex) in self.vertices.iter_mut().enumerate() {
            let normal = vertex.normal();
            let accumulated = accumulated[index];

            // Gram-Schmidt：把切线投影到与法线垂直的平面上。
            let tangent = (accumulated - normal * normal.dot(accumulated))
                .try_normalize()
                // 完全无法求出切线时，取一个与法线垂直的任意方向兜底。
                .unwrap_or_else(|| {
                    let axis = if normal.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
                    normal.cross(axis).normalize_or(Vec3::X)
                });

            // 手性：决定副切线取 +（N×T）还是 −（N×T），关系到法线贴图的凹凸方向。
            let handedness = if normal.cross(tangent).dot(bitangents[index]) < 0.0 {
                -1.0
            } else {
                1.0
            };

            vertex.tangent = [tangent.x, tangent.y, tangent.z, handedness];
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

    /// 造一个把所有顶点沿 +Y 推 `amount` 的形变目标。
    fn push_up(name: &str, count: usize, amount: f32) -> MorphTarget {
        MorphTarget::new(
            name,
            vec![
                MorphDelta {
                    position: [0.0, amount, 0.0],
                    normal: [0.0, 0.0, 0.0],
                    ..Default::default()
                };
                count
            ],
        )
    }

    #[test]
    fn meshes_have_no_morph_targets_by_default() {
        let mesh = Mesh::cube();

        assert!(!mesh.has_morph_targets());
        assert_eq!(mesh.morph_target_count(), 0);
        assert!(mesh.morph_weights().is_empty());
    }

    #[test]
    fn morph_targets_attach_with_their_weights() {
        let mesh = Mesh::cube();
        let count = mesh.vertices().len();

        let mesh = mesh.with_morph_targets(vec![push_up("mouth", count, 1.0)], vec![0.5]);

        assert!(mesh.has_morph_targets());
        assert_eq!(mesh.morph_target_count(), 1);
        assert_eq!(mesh.morph_weights(), &[0.5]);
        assert_eq!(mesh.find_morph_target("mouth"), Some(0));
        assert_eq!(mesh.find_morph_target("nope"), None);
    }

    #[test]
    fn mismatched_morph_targets_are_dropped() {
        let mesh = Mesh::cube();

        // 错位的增量会把网格撕开，丢掉这个形变比炸掉整个模型强。
        let mesh = mesh.with_morph_targets(vec![push_up("bad", 3, 1.0)], vec![1.0]);

        assert_eq!(mesh.morph_target_count(), 0);
        assert!(mesh.morph_weights().is_empty());
    }

    #[test]
    fn missing_weights_default_to_zero() {
        let mesh = Mesh::cube();
        let count = mesh.vertices().len();

        let mesh = mesh.with_morph_targets(
            vec![push_up("a", count, 1.0), push_up("b", count, 2.0)],
            vec![0.25],
        );

        assert_eq!(mesh.morph_weights(), &[0.25, 0.0]);
    }

    #[test]
    fn morphed_position_accumulates_weighted_deltas() {
        let mesh = Mesh::plane(1.0);
        let count = mesh.vertices().len();
        let base = mesh.vertices()[0].position();
        let mesh = mesh.with_morph_targets(
            vec![push_up("a", count, 1.0), push_up("b", count, 10.0)],
            vec![0.0, 0.0],
        );

        // 权重全零时形变不该改变任何东西。
        assert_eq!(mesh.morphed_position(0, &[0.0, 0.0]), base);
        // 两个目标按权重叠加：0.5×1 + 0.1×10 = 1.5。
        let morphed = mesh.morphed_position(0, &[0.5, 0.1]);
        assert!((morphed.y - base.y - 1.5).abs() < 1e-6);
    }

    #[test]
    fn morphed_position_tolerates_short_weight_slices() {
        let mesh = Mesh::plane(1.0);
        let count = mesh.vertices().len();
        let mesh = mesh.with_morph_targets(
            vec![push_up("a", count, 1.0), push_up("b", count, 1.0)],
            vec![0.0, 0.0],
        );

        // 只给了一个权重：另一个当作 0，而不是 panic。
        let morphed = mesh.morphed_position(0, &[1.0]);
        assert!((morphed.y - mesh.vertices()[0].position().y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn morphed_aabb_grows_with_the_weight() {
        let mesh = Mesh::plane(1.0);
        let count = mesh.vertices().len();
        let base = mesh.aabb();
        let mesh = mesh.with_morph_targets(vec![push_up("up", count, 2.0)], vec![0.0]);

        // 权重为 0 时包围盒不变。
        assert_eq!(mesh.morphed_aabb(&[0.0]), base);

        // 权重为 1 时纵向撑开 2。
        let grown = mesh.morphed_aabb(&[1.0]);
        assert!((grown.max.y - base.max.y - 2.0).abs() < 1e-5);
        // 保守起见两个方向都放开，剔除只怕小不怕大。
        assert!(grown.min.y <= base.min.y);
    }

    #[test]
    fn morphed_aabb_covers_the_actual_vertices() {
        let mesh = Mesh::plane(1.0);
        let count = mesh.vertices().len();
        let mesh = mesh.with_morph_targets(vec![push_up("up", count, 3.0)], vec![0.0]);

        let weights = [0.7];
        let bounds = mesh.morphed_aabb(&weights);
        for vertex in 0..count {
            let position = mesh.morphed_position(vertex, &weights);
            assert!(
                bounds.contains(position),
                "顶点 {position:?} 跑出了形变包围盒 {bounds:?}，会被误剔除"
            );
        }
    }

    #[test]
    fn negative_weights_also_expand_the_bounds() {
        let mesh = Mesh::plane(1.0);
        let count = mesh.vertices().len();
        let mesh = mesh.with_morph_targets(vec![push_up("up", count, 1.0)], vec![0.0]);

        // 权重可以是负的（反向外插），包围盒同样要放开。
        let bounds = mesh.morphed_aabb(&[-1.0]);
        for vertex in 0..count {
            assert!(bounds.contains(mesh.morphed_position(vertex, &[-1.0])));
        }
    }

    #[test]
    fn mesh_without_morph_targets_keeps_its_aabb() {
        let mesh = Mesh::cube();

        assert_eq!(mesh.morphed_aabb(&[]), mesh.aabb());
    }

    #[test]
    fn morph_delta_layout_is_gpu_ready() {
        // position(12) + 填充(4) + normal(12) + 填充(4)。
        // WGSL 的 vec3 对齐是 16，不补齐两边字段就会错位。
        assert_eq!(size_of::<MorphDelta>(), 32);
        assert_eq!(size_of::<MorphDelta>() % 16, 0);
    }

    #[test]
    fn meshes_are_static_by_default() {
        let mesh = Mesh::cube();

        assert!(!mesh.is_skinned());
        assert!(mesh.skin().is_none());
    }

    #[test]
    fn skin_attaches_when_counts_match() {
        let mesh = Mesh::cube();
        let count = mesh.vertices().len();

        let mesh = mesh.with_skin(vec![SkinVertex::default(); count]);

        assert!(mesh.is_skinned());
        assert_eq!(mesh.skin().unwrap().len(), count);
    }

    #[test]
    fn mismatched_skin_is_rejected() {
        // 错位的关节下标会让整个模型炸开，宁可退化成静态网格。
        let mesh = Mesh::cube().with_skin(vec![SkinVertex::default(); 3]);

        assert!(!mesh.is_skinned());
    }

    #[test]
    fn skin_weights_are_normalised_on_attach() {
        let mesh = Mesh::plane(1.0);
        let count = mesh.vertices().len();
        let unnormalised = SkinVertex {
            joints: [0, 1, 2, 3],
            weights: [2.0, 2.0, 0.0, 0.0],
        };

        let mesh = mesh.with_skin(vec![unnormalised; count]);

        let sum = mesh.skin().unwrap()[0].weight_sum();
        assert!((sum - 1.0).abs() < 1e-6, "权重和为 {sum}，没有归一化");
    }

    #[test]
    fn zero_weights_fall_back_to_rigid_binding() {
        let mut vertex = SkinVertex {
            joints: [5, 6, 7, 8],
            weights: [0.0; 4],
        };

        vertex.normalize();

        // 权重全零时顶点会塌到原点，退回刚性绑定才是安全的。
        assert_eq!(vertex.weights, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(vertex.joints, [0; 4]);
    }

    #[test]
    fn skin_vertex_layout_is_compact() {
        // joints(8) + weights(16)。顶点缓冲的跨距按它算，改了要同步渲染器。
        assert_eq!(size_of::<SkinVertex>(), 24);
    }

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
    fn tangents_align_with_uv_direction() {
        // XY 平面上的三角形，U 沿 +X、V 沿 +Y，切线应当指向 +X。
        let vertices = vec![
            Vertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                ..Default::default()
            },
            Vertex {
                position: [1.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [1.0, 0.0],
                ..Default::default()
            },
            Vertex {
                position: [0.0, 1.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 1.0],
                ..Default::default()
            },
        ];
        let mut mesh = Mesh::new(vertices, vec![0, 1, 2]);

        mesh.recompute_tangents();

        for vertex in mesh.vertices() {
            assert!(
                (vertex.tangent() - Vec3::X).length() < 1e-4,
                "切线应指向 +X，实得 {:?}",
                vertex.tangent()
            );
        }
    }

    #[test]
    fn tangents_are_orthogonal_to_normals() {
        // 切线必须垂直于法线，否则构建出的切线空间是斜的，法线贴图会歪。
        let mut mesh = Mesh::cube();
        mesh.recompute_tangents();

        for vertex in mesh.vertices() {
            let dot = vertex.normal().dot(vertex.tangent());
            assert!(dot.abs() < 1e-4, "切线与法线不正交：dot = {dot}");
            assert!((vertex.tangent().length() - 1.0).abs() < 1e-4, "切线未归一化");
        }
    }

    #[test]
    fn degenerate_uv_does_not_produce_nan() {
        // 三个顶点 UV 完全相同，UV 面积为零，无法定出切线方向。
        let vertices = vec![
            Vertex {
                position: [0.0, 0.0, 0.0],
                uv: [0.5, 0.5],
                ..Default::default()
            },
            Vertex {
                position: [1.0, 0.0, 0.0],
                uv: [0.5, 0.5],
                ..Default::default()
            },
            Vertex {
                position: [0.0, 1.0, 0.0],
                uv: [0.5, 0.5],
                ..Default::default()
            },
        ];
        let mut mesh = Mesh::new(vertices, vec![0, 1, 2]);

        mesh.recompute_tangents();

        for vertex in mesh.vertices() {
            assert!(vertex.tangent().is_finite(), "退化 UV 产生了 NaN");
            // 兜底切线仍需垂直于法线。
            assert!(vertex.normal().dot(vertex.tangent()).abs() < 1e-4);
        }
    }

    #[test]
    fn tangent_handedness_is_recorded() {
        let mut mesh = Mesh::cube();
        mesh.recompute_tangents();

        // 手性只能是 ±1，其他值说明计算出了问题。
        for vertex in mesh.vertices() {
            assert!(vertex.tangent[3] == 1.0 || vertex.tangent[3] == -1.0);
        }
    }

    #[test]
    fn recompute_tangents_ignores_out_of_range_indices() {
        let mut mesh = Mesh::new(vec![Vertex::default()], vec![0, 7, 9]);

        mesh.recompute_tangents();

        assert!(mesh.vertices()[0].tangent().is_finite());
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
