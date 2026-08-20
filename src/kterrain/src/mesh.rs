//! 由高度图生成网格，含分块与 LOD。
//!
//! # 为什么要分块
//!
//! 一块 1024×1024 的地形有两百万个三角形。整块画的话既不能剔除
//! （视锥里只有一小片也要提交全部），也不能按距离降细节。
//! 切成 [`Chunk`] 之后两者都能做。
//!
//! # LOD 与裂缝
//!
//! 远处的块按 2 的幂降采样：`step = 2^lod`。相邻两块 LOD 不同时，
//! 边界上细的一侧比粗的一侧多出一些顶点——那些顶点不在粗侧的直线上，
//! 于是两块之间**裂开一条缝**，能透过去看到天空。这是地形 LOD 最经典的坑。
//!
//! 这里用**缝合裙边**（skirt）之外的另一条路：细的一侧在边界上
//! **强制对齐到粗侧的采样点**（见 [`Chunk::build`] 里的 `stitch`）。
//! 裙边是往下拉一圈垂直面把缝挡住，简单但会在陡坡上露出来；
//! 对齐没有额外几何，代价是要知道四个邻居的 LOD。

use crate::Heightmap;
use kmath::{Vec2, Vec3};
use kmesh::{Mesh, Vertex};

/// 一块地形在四个方向上的邻居 LOD。
///
/// 用于边界对齐。`None` 表示那个方向没有邻居（地形边缘），不必对齐。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NeighborLods {
    /// -Z 方向。
    pub north: Option<u32>,
    /// +Z 方向。
    pub south: Option<u32>,
    /// -X 方向。
    pub west: Option<u32>,
    /// +X 方向。
    pub east: Option<u32>,
}

/// 一块地形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// 左上角顶点在高度图里的列号。
    pub col: usize,
    /// 左上角顶点在高度图里的行号。
    pub row: usize,
    /// 沿 X 覆盖多少个格子。
    ///
    /// X 和 Z 分开：地形的格子数不是块大小的整数倍时，边角上会出现
    /// **长方形**的残块。强行按正方形算的话，那些残块会按短边截断，
    /// 长边多出来的格子整块丢掉——地形边缘缺一条。
    pub cells_x: usize,
    /// 沿 Z 覆盖多少个格子。
    pub cells_z: usize,
}

impl Chunk {
    /// 这一块在地形局部坐标里的 XZ 范围。
    pub fn bounds(&self, map: &Heightmap) -> (Vec2, Vec2) {
        let cell = map.cell_size();
        let min = Vec2::new(self.col as f32 * cell.x, self.row as f32 * cell.y);
        let max = min + Vec2::new(self.cells_x as f32 * cell.x, self.cells_z as f32 * cell.y);
        (min, max)
    }

    /// 这一块的中心（含高度）。LOD 按到它的距离选。
    pub fn center(&self, map: &Heightmap) -> Vec3 {
        let (min, max) = self.bounds(map);
        let center = (min + max) * 0.5;
        Vec3::new(center.x, map.sample(center.x, center.y), center.y)
    }

    /// 生成这一块的网格。
    ///
    /// `lod` 为 0 是全精度，每加 1 采样步长翻倍。
    /// `neighbors` 用来做边界对齐，防止和 LOD 更粗的邻居之间裂开。
    pub fn build(&self, map: &Heightmap, lod: u32, neighbors: NeighborLods) -> Mesh {
        let step = 1usize << lod;
        // 步长不能超过这一块的格子数，否则一个三角形都生成不出来。
        let step = step.min(self.cells_x.max(1)).min(self.cells_z.max(1));
        let side_x = (self.cells_x / step).max(1);
        let side_z = (self.cells_z / step).max(1);
        let cell = map.cell_size();

        let mut vertices = Vec::with_capacity((side_x + 1) * (side_z + 1));
        for j in 0..=side_z {
            for i in 0..=side_x {
                // 边界对齐：邻居更粗时，把细侧多出来的顶点吸附到
                // 粗侧的采样点上。不做的话两块之间会裂开一条缝。
                let (i, j) = stitch(i, j, side_x, side_z, lod, neighbors);

                let col = self.col + i * step;
                let row = self.row + j * step;
                let x = col as f32 * cell.x;
                let z = row as f32 * cell.y;

                vertices.push(Vertex::new(
                    Vec3::new(x, map.height(col, row), z),
                    map.normal(x, z),
                    // UV 铺满整块地形，方便贴 splat 图。
                    [x / map.size().x, z / map.size().y],
                ));
            }
        }

        let mut indices = Vec::with_capacity(side_x * side_z * 6);
        for j in 0..side_z {
            for i in 0..side_x {
                let a = (j * (side_x + 1) + i) as u32;
                let b = a + 1;
                let c = a + (side_x + 1) as u32;
                let d = c + 1;
                // 绕序要和引擎的正面一致（逆时针为正面），
                // 反了的话整块地形会被背面剔除掉，什么都看不见。
                indices.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }

        Mesh::new(vertices, indices)
    }
}

/// 把边界上的采样下标吸附到更粗的邻居的采样点上。
///
/// 只动**边界**上的顶点，内部不动。吸附的做法是把下标向下取整到
/// 邻居步长的整数倍——那正好是邻居实际采样的位置，于是两侧的
/// 顶点重合，缝就没了。
fn stitch(
    i: usize,
    j: usize,
    side_x: usize,
    side_z: usize,
    lod: u32,
    neighbors: NeighborLods,
) -> (usize, usize) {
    let mut i = i;
    let mut j = j;

    // 邻居比自己粗多少级。粗 n 级意味着它每 2^n 个格子才采一个点。
    let coarser = |neighbor: Option<u32>| -> usize {
        match neighbor {
            Some(n) if n > lod => 1usize << (n - lod),
            _ => 1,
        }
    };

    if j == 0 {
        let ratio = coarser(neighbors.north);
        i = i / ratio * ratio;
    } else if j == side_z {
        let ratio = coarser(neighbors.south);
        i = i / ratio * ratio;
    }

    if i == 0 {
        let ratio = coarser(neighbors.west);
        j = j / ratio * ratio;
    } else if i == side_x {
        let ratio = coarser(neighbors.east);
        j = j / ratio * ratio;
    }

    (i, j)
}

/// 把一张高度图切成若干块。
///
/// `cells_per_chunk` 是每块每边的格子数，应当是 2 的幂——
/// LOD 靠不断折半，不是 2 的幂的话降到某一级会除不尽，
/// 边界对不上。
pub fn split(map: &Heightmap, cells_per_chunk: usize) -> Vec<Chunk> {
    let cells_per_chunk = cells_per_chunk.max(1);
    let cols = map.cols() - 1; // 格子数
    let rows = map.rows() - 1;

    let mut chunks = Vec::new();
    let mut row = 0;
    while row < rows {
        let mut col = 0;
        while col < cols {
            chunks.push(Chunk {
                col,
                row,
                // 边角上的残块两个方向各自截断。取两者的 min 会
                // 把长边多出来的格子整块丢掉，地形边缘缺一条。
                cells_x: cells_per_chunk.min(cols - col),
                cells_z: cells_per_chunk.min(rows - row),
            });
            col += cells_per_chunk;
        }
        row += cells_per_chunk;
    }
    chunks
}

/// 按到相机的距离选 LOD。
///
/// `distances` 是各级 LOD 的**上界**，按从近到远排列：
/// `[50, 150, 400]` 表示 50 米内用 LOD 0，50~150 用 1，
/// 150~400 用 2，再远用 3。
pub fn lod_for(distance: f32, distances: &[f32]) -> u32 {
    distances
        .iter()
        .position(|d| distance < *d)
        .unwrap_or(distances.len()) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张 17×17 顶点（16×16 格）、覆盖 160×160 米的图。
    fn map() -> Heightmap {
        let mut map = Heightmap::flat(17, 17, Vec2::new(160.0, 160.0));
        for row in 0..map.rows() {
            for col in 0..map.cols() {
                map.set_height(col, row, (col as f32 * 0.4).sin() * 5.0 + row as f32 * 0.3);
            }
        }
        map
    }

    fn full() -> Chunk {
        Chunk {
            col: 0,
            row: 0,
            cells_x: 16,
            cells_z: 16,
        }
    }

    #[test]
    fn lod_zero_uses_every_vertex() {
        let mesh = full().build(&map(), 0, NeighborLods::default());
        // 16 格 → 17×17 个顶点。
        assert_eq!(mesh.vertices().len(), 17 * 17);
        assert_eq!(mesh.indices().len(), 16 * 16 * 6);
    }

    #[test]
    fn each_lod_level_halves_the_resolution() {
        let map = map();
        let counts: Vec<usize> = (0..4)
            .map(|lod| full().build(&map, lod, NeighborLods::default()).vertices().len())
            .collect();
        assert_eq!(counts, vec![17 * 17, 9 * 9, 5 * 5, 3 * 3]);
    }

    #[test]
    fn an_absurd_lod_still_produces_a_triangle() {
        // 步长超过块的格子数时，一个三角形都生成不出来——
        // 空网格会让这一块凭空消失。
        let mesh = full().build(&map(), 20, NeighborLods::default());
        assert!(mesh.indices().len() >= 6);
        assert!(!mesh.vertices().is_empty());
    }

    #[test]
    fn indices_stay_in_range() {
        let map = map();
        for lod in 0..4 {
            let mesh = full().build(&map, lod, NeighborLods::default());
            let count = mesh.vertices().len() as u32;
            assert!(
                mesh.indices().iter().all(|i| *i < count),
                "LOD {lod} 的索引越界了"
            );
        }
    }

    #[test]
    fn vertices_sit_on_the_heightmap() {
        let map = map();
        let mesh = full().build(&map, 0, NeighborLods::default());
        for v in mesh.vertices() {
            let expected = map.sample(v.position[0], v.position[2]);
            assert!(
                (v.position[1] - expected).abs() < 1e-4,
                "顶点没落在高度图上：{} vs {expected}",
                v.position[1]
            );
        }
    }

    #[test]
    fn uvs_span_the_whole_terrain() {
        let map = map();
        let mesh = full().build(&map, 0, NeighborLods::default());
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for v in mesh.vertices() {
            lo = lo.min(v.uv[0]);
            hi = hi.max(v.uv[0]);
        }
        assert!((lo - 0.0).abs() < 1e-5);
        assert!((hi - 1.0).abs() < 1e-5);
    }

    #[test]
    fn winding_is_counter_clockwise_when_seen_from_above() {
        // 绕序反了的话整块地形会被背面剔除，屏幕上什么都没有，
        // 而且不报任何错。
        let map = Heightmap::flat(3, 3, Vec2::new(20.0, 20.0));
        let mesh = Chunk {
            col: 0,
            row: 0,
            cells_x: 2,
            cells_z: 2,
        }
        .build(&map, 0, NeighborLods::default());

        let v = mesh.vertices();
        for tri in mesh.indices().chunks(3) {
            let a = Vec3::from_array(v[tri[0] as usize].position);
            let b = Vec3::from_array(v[tri[1] as usize].position);
            let c = Vec3::from_array(v[tri[2] as usize].position);
            let normal = (b - a).cross(c - a);
            assert!(normal.y > 0.0, "三角形朝下了：法线 {normal:?}");
        }
    }

    #[test]
    fn a_coarser_neighbor_makes_the_edges_line_up() {
        // 这是地形 LOD 最经典的坑：细的一侧比粗的一侧多出顶点，
        // 那些顶点不在粗侧的直线上，两块之间裂开一条能看到天的缝。
        let map = map();

        let fine = Chunk {
            col: 0,
            row: 0,
            cells_x: 8,
            cells_z: 8,
        };
        let coarse = Chunk {
            col: 8,
            row: 0,
            cells_x: 8,
            cells_z: 8,
        };

        // 细块用 LOD 0，右邻居是 LOD 1。
        let fine_mesh = fine.build(
            &map,
            0,
            NeighborLods {
                east: Some(1),
                ..Default::default()
            },
        );
        let coarse_mesh = coarse.build(&map, 1, NeighborLods::default());

        // 取两块共享的那条边（x = 80）上的顶点。
        let edge_of = |mesh: &Mesh| -> Vec<Vec3> {
            let mut points: Vec<Vec3> = mesh
                .vertices()
                .iter()
                .map(|v| Vec3::from_array(v.position))
                .filter(|p| (p.x - 80.0).abs() < 1e-3)
                .collect();
            points.sort_by(|a, b| a.z.total_cmp(&b.z));
            points.dedup_by(|a, b| (a.z - b.z).abs() < 1e-3);
            points
        };

        let fine_edge = edge_of(&fine_mesh);
        let coarse_edge = edge_of(&coarse_mesh);
        assert!(!fine_edge.is_empty() && !coarse_edge.is_empty());

        // 细侧边界上每一个不同的 z 都必须在粗侧也有，高度还得一样。
        for point in &fine_edge {
            let matched = coarse_edge
                .iter()
                .any(|c| (c.z - point.z).abs() < 1e-3 && (c.y - point.y).abs() < 1e-3);
            assert!(matched, "细侧的边界点 {point:?} 在粗侧找不到，会裂缝");
        }
    }

    #[test]
    fn stitching_leaves_the_interior_alone() {
        // 只该动边界。动了内部的话地形表面会出现一片扭曲。
        let map = map();
        let plain = full().build(&map, 0, NeighborLods::default());
        let stitched = full().build(
            &map,
            0,
            NeighborLods {
                east: Some(2),
                north: Some(2),
                ..Default::default()
            },
        );

        let side = 16;
        for j in 1..side {
            for i in 1..side {
                let index = j * (side + 1) + i;
                assert_eq!(
                    plain.vertices()[index].position,
                    stitched.vertices()[index].position,
                    "内部顶点 ({i},{j}) 被缝合动过了"
                );
            }
        }
    }

    #[test]
    fn a_finer_neighbor_needs_no_stitching() {
        // 只有比自己**粗**的邻居才要对齐。细的那侧会自己来贴合。
        let map = map();
        let a = full().build(&map, 1, NeighborLods::default());
        let b = full().build(
            &map,
            1,
            NeighborLods {
                east: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(a.vertices().len(), b.vertices().len());
        for (x, y) in a.vertices().iter().zip(b.vertices()) {
            assert_eq!(x.position, y.position);
        }
    }

    #[test]
    fn splitting_covers_every_cell_exactly_once() {
        let map = map();
        let chunks = split(&map, 8);
        assert_eq!(chunks.len(), 4, "16×16 格切成 8×8 该是四块");

        let covered: usize = chunks.iter().map(|c| c.cells_x * c.cells_z).sum();
        assert_eq!(covered, 16 * 16);
    }

    #[test]
    fn splitting_handles_a_ragged_last_chunk() {
        // 格子数不是块大小的整数倍时，最后一块要截断而不是越界。
        let map = Heightmap::flat(11, 11, Vec2::new(100.0, 100.0)); // 10×10 格
        let chunks = split(&map, 4);
        for chunk in &chunks {
            assert!(chunk.col + chunk.cells_x <= 10);
            assert!(chunk.row + chunk.cells_z <= 10);
            assert!(chunk.cells_x > 0 && chunk.cells_z > 0);
        }
        let covered: usize = chunks.iter().map(|c| c.cells_x * c.cells_z).sum();
        assert_eq!(covered, 10 * 10, "边角上的残块丢了格子");
    }

    #[test]
    fn chunk_bounds_tile_without_gaps() {
        let map = map();
        let chunks = split(&map, 8);
        let (_, first_max) = chunks[0].bounds(&map);
        let (second_min, _) = chunks[1].bounds(&map);
        assert_eq!(first_max.x, second_min.x, "相邻块之间不该有空隙");
    }

    #[test]
    fn lod_is_chosen_by_distance() {
        let bands = [50.0, 150.0, 400.0];
        assert_eq!(lod_for(10.0, &bands), 0);
        assert_eq!(lod_for(50.0, &bands), 1, "边界值该落进下一级");
        assert_eq!(lod_for(149.0, &bands), 1);
        assert_eq!(lod_for(399.0, &bands), 2);
        assert_eq!(lod_for(10_000.0, &bands), 3);
    }

    #[test]
    fn no_lod_bands_means_full_detail() {
        assert_eq!(lod_for(9999.0, &[]), 0);
    }
}
