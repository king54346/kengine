//! 一整块地形：高度图 + 材质混合 + 分块 + LOD 状态。
//!
//! 这是对外的门面。它记住每块**当前**的 LOD，[`Terrain::update`] 按相机
//! 位置算出新 LOD 并只报告**变了的**那些——重建网格是有代价的，
//! 每帧无脑重建全部块会把 CPU 吃光。

use crate::{Chunk, Heightmap, NeighborLods, SplatMap, lod_for, split};
use kmath::{Aabb, Vec3};
use kmesh::Mesh;

/// 一整块地形。
#[derive(Debug, Clone)]
pub struct Terrain {
    heightmap: Heightmap,
    splat: SplatMap,
    chunks: Vec<Chunk>,
    /// 每块当前的 LOD，与 `chunks` 一一对应。
    lods: Vec<u32>,
    /// LOD 的距离分档。
    lod_distances: Vec<f32>,
    /// 每块多少格。
    cells_per_chunk: usize,
    /// 高度图被改过，网格需要重建。
    dirty: bool,
}

/// 一块需要重建网格的地形块。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkUpdate {
    /// 在 [`Terrain::chunks`] 里的下标。
    pub index: usize,
    /// 新的 LOD。
    pub lod: u32,
}

impl Terrain {
    /// 建一块地形。
    ///
    /// `cells_per_chunk` 应当是 2 的幂：LOD 靠不断折半，不是 2 的幂的话
    /// 降到某一级会除不尽，块与块的边界对不上。
    pub fn new(heightmap: Heightmap, cells_per_chunk: usize, layers: usize) -> Self {
        let chunks = split(&heightmap, cells_per_chunk);
        let splat = SplatMap::new(heightmap.cols(), heightmap.rows(), layers);
        Self {
            lods: vec![u32::MAX; chunks.len()],
            chunks,
            heightmap,
            splat,
            // 默认三档。想关掉 LOD 就传空数组。
            lod_distances: vec![120.0, 300.0, 700.0],
            cells_per_chunk,
            dirty: true,
        }
    }

    /// 高度图。
    pub fn heightmap(&self) -> &Heightmap {
        &self.heightmap
    }

    /// 高度图的可变引用。
    ///
    /// 改完会把整块地形标记为脏——调用方拿了可变引用之后引擎无从知道
    /// 改了哪里，只能整体重建。批量编辑时应当一次拿、改完再放。
    pub fn heightmap_mut(&mut self) -> &mut Heightmap {
        self.dirty = true;
        &mut self.heightmap
    }

    /// 材质混合图。
    pub fn splat(&self) -> &SplatMap {
        &self.splat
    }

    /// 材质混合图的可变引用。
    pub fn splat_mut(&mut self) -> &mut SplatMap {
        self.dirty = true;
        &mut self.splat
    }

    /// 全部分块。
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// 某一块当前的 LOD。
    pub fn lod(&self, index: usize) -> u32 {
        self.lods.get(index).copied().unwrap_or(0)
    }

    /// 设置 LOD 的距离分档。空数组表示永远用全精度。
    pub fn set_lod_distances(&mut self, distances: Vec<f32>) {
        self.lod_distances = distances;
        // 分档变了，所有块的 LOD 都要重算。
        self.lods.fill(u32::MAX);
    }

    /// 整块地形的包围盒（局部坐标）。
    pub fn bounds(&self) -> Aabb {
        let (lo, hi) = self.heightmap.height_range();
        let size = self.heightmap.size();
        Aabb::new(Vec3::new(0.0, lo, 0.0), Vec3::new(size.x, hi, size.y))
    }

    /// 一条射线与地形的交点（局部坐标）。笔刷落点靠它。
    pub fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Vec3> {
        self.heightmap.raycast(origin, direction, max_distance)
    }

    /// 按相机位置更新 LOD，返回需要重建网格的块。
    ///
    /// `camera` 是**地形局部坐标**里的相机位置。
    ///
    /// 只报告变了的块。每帧无脑重建全部块的话，一块 1024² 的地形
    /// 每帧要生成两百万个三角形，CPU 直接跑满。
    pub fn update(&mut self, camera: Vec3) -> Vec<ChunkUpdate> {
        let mut updates = Vec::new();
        let dirty = self.dirty;

        for index in 0..self.chunks.len() {
            let distance = self.chunks[index].center(&self.heightmap).distance(camera);
            let lod = lod_for(distance, &self.lod_distances);
            if dirty || self.lods[index] != lod {
                self.lods[index] = lod;
                updates.push(ChunkUpdate { index, lod });
            }
        }

        // 有块换了 LOD 的话，它的邻居也要重建——邻居的边界要对齐到
        // 新的 LOD 上，不重建就会在两块之间裂开一条缝。
        if !dirty {
            let changed: Vec<usize> = updates.iter().map(|u| u.index).collect();
            for index in 0..self.chunks.len() {
                if changed.contains(&index) {
                    continue;
                }
                if self
                    .neighbor_indices(index)
                    .into_iter()
                    .flatten()
                    .any(|n| changed.contains(&n))
                {
                    updates.push(ChunkUpdate {
                        index,
                        lod: self.lods[index],
                    });
                }
            }
        }

        self.dirty = false;
        updates
    }

    /// 生成某一块的网格，含边界对齐。
    pub fn build_chunk(&self, index: usize) -> Option<Mesh> {
        let chunk = self.chunks.get(index)?;
        Some(chunk.build(&self.heightmap, self.lods[index], self.neighbors(index)))
    }

    /// 某一块的四个邻居的 LOD。
    fn neighbors(&self, index: usize) -> NeighborLods {
        let [north, south, west, east] = self.neighbor_indices(index);
        NeighborLods {
            north: north.map(|i| self.lods[i]),
            south: south.map(|i| self.lods[i]),
            west: west.map(|i| self.lods[i]),
            east: east.map(|i| self.lods[i]),
        }
    }

    /// 四个方向上的邻居下标，顺序是 `[北, 南, 西, 东]`。
    fn neighbor_indices(&self, index: usize) -> [Option<usize>; 4] {
        let per_row = self.chunks_per_row();
        if per_row == 0 || index >= self.chunks.len() {
            return [None; 4];
        }
        let (col, row) = (index % per_row, index / per_row);
        let rows = self.chunks.len().div_ceil(per_row);

        let at = |c: usize, r: usize| -> Option<usize> {
            (c < per_row && r < rows).then(|| r * per_row + c)?.into()
        };
        [
            row.checked_sub(1).and_then(|r| at(col, r)),
            at(col, row + 1).filter(|i| *i < self.chunks.len()),
            col.checked_sub(1).and_then(|c| at(c, row)),
            at(col + 1, row).filter(|i| *i < self.chunks.len()),
        ]
    }

    /// 一行有多少块。
    fn chunks_per_row(&self) -> usize {
        let cols = self.heightmap.cols() - 1;
        cols.div_ceil(self.cells_per_chunk.max(1))
    }

    /// 地形的高度场碰撞体参数：`(行数, 列数, 高度值, 缩放)`。
    ///
    /// rapier 的高度场把 `scale` 的 X/Z 当作**整体覆盖范围**，
    /// 而且原点在正中而不是角上——直接把地形原点当碰撞体原点的话，
    /// 物理里的地面会比画面上的偏半块。调用方要把碰撞体挪到
    /// [`Terrain::collider_offset`]。
    pub fn collider_data(&self) -> (usize, usize, Vec<f32>, Vec3) {
        let size = self.heightmap.size();
        (
            self.heightmap.rows(),
            self.heightmap.cols(),
            self.heightmap.heights().to_vec(),
            // Y 给 1：高度值本身已经是米，再乘一次会把地形拉高一倍。
            Vec3::new(size.x, 1.0, size.y),
        )
    }

    /// 高度场碰撞体相对地形原点的偏移。
    ///
    /// rapier 的高度场以**中心**为原点，地形却以角为原点，差半块。
    /// 不补这个偏移，角色会踩在离视觉地面半个地形远的地方。
    pub fn collider_offset(&self) -> Vec3 {
        let size = self.heightmap.size();
        Vec3::new(size.x * 0.5, 0.0, size.y * 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kmath::Vec2;

    /// 33×33 顶点（32×32 格），320×320 米，切成 8 格一块 → 4×4 = 16 块。
    fn terrain() -> Terrain {
        let mut map = Heightmap::flat(33, 33, Vec2::new(320.0, 320.0));
        for row in 0..map.rows() {
            for col in 0..map.cols() {
                map.set_height(col, row, (col as f32 * 0.3).sin() * 8.0);
            }
        }
        Terrain::new(map, 8, 3)
    }

    #[test]
    fn the_chunk_grid_has_the_expected_shape() {
        let terrain = terrain();
        assert_eq!(terrain.chunks().len(), 16);
        assert_eq!(terrain.chunks_per_row(), 4);
    }

    #[test]
    fn the_first_update_reports_every_chunk() {
        // 一块都不报的话，地形第一帧是空的。
        let mut terrain = terrain();
        let updates = terrain.update(Vec3::ZERO);
        assert_eq!(updates.len(), 16);
    }

    #[test]
    fn a_stationary_camera_produces_no_further_updates() {
        // 每帧无脑重建全部块的话，一块 1024² 的地形每帧要生成
        // 两百万个三角形，CPU 直接跑满。
        let mut terrain = terrain();
        terrain.update(Vec3::new(160.0, 50.0, 160.0));
        let second = terrain.update(Vec3::new(160.0, 50.0, 160.0));
        assert!(second.is_empty(), "相机没动却报了 {} 块", second.len());
    }

    #[test]
    fn moving_the_camera_changes_some_lods() {
        let mut terrain = terrain();
        terrain.update(Vec3::new(0.0, 0.0, 0.0));
        let updates = terrain.update(Vec3::new(320.0, 0.0, 320.0));
        assert!(!updates.is_empty(), "相机跑到对角却一块都没换 LOD");
    }

    #[test]
    fn nearby_chunks_get_finer_lods_than_distant_ones() {
        let mut terrain = terrain();
        terrain.update(Vec3::new(0.0, 0.0, 0.0));

        let near = terrain.lod(0);
        let far = terrain.lod(15);
        assert!(near < far, "近处 LOD {near}，远处 LOD {far}");
    }

    #[test]
    fn changing_a_chunk_also_rebuilds_its_neighbors() {
        // 邻居的边界要对齐到新的 LOD 上；不重建的话两块之间会裂缝。
        let mut terrain = terrain();
        terrain.update(Vec3::new(0.0, 0.0, 0.0));

        // 稍微挪一下相机，只让少数几块换档。
        let updates = terrain.update(Vec3::new(40.0, 0.0, 0.0));
        if updates.is_empty() {
            return;
        }

        let reported: Vec<usize> = updates.iter().map(|u| u.index).collect();
        // 被报告的块里，至少有一块的邻居也在名单上。
        let has_neighbor = reported.iter().any(|index| {
            terrain
                .neighbor_indices(*index)
                .into_iter()
                .flatten()
                .any(|n| reported.contains(&n))
        });
        assert!(has_neighbor, "换了 LOD 的块的邻居没被一起重建");
    }

    #[test]
    fn editing_the_heightmap_rebuilds_everything() {
        let mut terrain = terrain();
        terrain.update(Vec3::ZERO);
        assert!(terrain.update(Vec3::ZERO).is_empty());

        terrain.heightmap_mut().set_height(5, 5, 30.0);
        assert_eq!(
            terrain.update(Vec3::ZERO).len(),
            16,
            "改了高度图之后该整体重建"
        );
    }

    #[test]
    fn changing_the_lod_bands_rebuilds_everything() {
        let mut terrain = terrain();
        terrain.update(Vec3::ZERO);
        terrain.set_lod_distances(vec![10.0]);
        assert_eq!(terrain.update(Vec3::ZERO).len(), 16);
    }

    #[test]
    fn empty_lod_bands_mean_full_detail_everywhere() {
        let mut terrain = terrain();
        terrain.set_lod_distances(Vec::new());
        terrain.update(Vec3::new(9999.0, 0.0, 9999.0));
        assert!((0..16).all(|i| terrain.lod(i) == 0));
    }

    #[test]
    fn every_chunk_builds_a_non_empty_mesh() {
        let mut terrain = terrain();
        terrain.update(Vec3::new(160.0, 100.0, 160.0));
        for index in 0..terrain.chunks().len() {
            let mesh = terrain.build_chunk(index).expect("每块都该能生成网格");
            assert!(!mesh.indices().is_empty(), "第 {index} 块是空的");
        }
    }

    #[test]
    fn neighbor_indices_stay_inside_the_grid() {
        let terrain = terrain();
        for index in 0..terrain.chunks().len() {
            for neighbor in terrain.neighbor_indices(index).into_iter().flatten() {
                assert!(neighbor < terrain.chunks().len());
                assert_ne!(neighbor, index, "自己不是自己的邻居");
            }
        }
    }

    #[test]
    fn corner_chunks_have_two_neighbors() {
        let terrain = terrain();
        let count = |index: usize| {
            terrain
                .neighbor_indices(index)
                .into_iter()
                .flatten()
                .count()
        };
        assert_eq!(count(0), 2, "左上角该只有东、南两个邻居");
        assert_eq!(count(5), 4, "内部的块该有四个邻居");
    }

    #[test]
    fn the_bounds_cover_the_whole_terrain() {
        let terrain = terrain();
        let bounds = terrain.bounds();
        let (lo, hi) = terrain.heightmap().height_range();
        assert_eq!(bounds.min, Vec3::new(0.0, lo, 0.0));
        assert_eq!(bounds.max, Vec3::new(320.0, hi, 320.0));
    }

    #[test]
    fn the_collider_is_offset_to_the_terrain_center() {
        // rapier 的高度场以中心为原点，地形以角为原点。不补偏移的话，
        // 角色会踩在离视觉地面半个地形远的地方。
        let terrain = terrain();
        assert_eq!(terrain.collider_offset(), Vec3::new(160.0, 0.0, 160.0));
    }

    #[test]
    fn the_collider_scale_does_not_double_the_height() {
        // Y 给成高度范围的话，地形会被拉高一倍。高度值本身已经是米。
        let terrain = terrain();
        let (rows, cols, heights, scale) = terrain.collider_data();
        assert_eq!(rows, 33);
        assert_eq!(cols, 33);
        assert_eq!(heights.len(), 33 * 33);
        assert_eq!(scale, Vec3::new(320.0, 1.0, 320.0));
    }

    #[test]
    fn raycasting_hits_the_surface() {
        let terrain = terrain();
        let hit = terrain
            .raycast(Vec3::new(160.0, 200.0, 160.0), Vec3::NEG_Y, 400.0)
            .expect("垂直往下该打中");
        let surface = terrain.heightmap().sample(hit.x, hit.z);
        assert!((hit.y - surface).abs() < 0.05);
    }
}
