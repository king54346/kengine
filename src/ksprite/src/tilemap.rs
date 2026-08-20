//! 瓦片地图。
//!
//! # 为什么不是一堆精灵节点
//!
//! 一张 200×200 的地图有四万格。每格一个场景节点的话，光是节点本身
//! 就吃掉几十兆，而且每帧要对四万个节点做变换传播和剔除。
//!
//! 这里的做法是：地图是**一个**节点，[`TileMap::build`] 把可见范围内的
//! 瓦片直接合成**一块网格**。四万格变成一次绘制调用。
//!
//! # 空瓦片不产生几何
//!
//! 地图里大部分格子通常是空的（[`TileMap::EMPTY`]）。给空格也生成方片的话，
//! 顶点数是实际需要的好几倍，而且它们全透明——既浪费带宽又给混合添乱。
//!
//! # 只合成可见范围
//!
//! [`TileMap::build`] 接一个范围参数。整张图一次合成的话，
//! 一张 1000×1000 的地图会生成四百万个顶点，而屏幕上只看得见几百格。

use crate::{Atlas, SpriteRegion};
use kmath::{Vec2, Vec3};
use kmesh::{Mesh, Vertex};

/// 一张瓦片地图。
#[derive(Debug, Clone, PartialEq)]
pub struct TileMap {
    cols: usize,
    rows: usize,
    /// 每格的世界尺寸。
    tile_size: Vec2,
    /// 行主序的瓦片编号，[`TileMap::EMPTY`] 表示空。
    tiles: Vec<u32>,
}

impl TileMap {
    /// 空瓦片的编号。
    ///
    /// 用 `u32::MAX` 而不是 0：0 是一个完全合法的瓦片号，
    /// 拿它当「空」的话图集的第一格永远画不出来。
    pub const EMPTY: u32 = u32::MAX;

    /// 一张全空的地图。
    pub fn new(cols: usize, rows: usize, tile_size: Vec2) -> Self {
        Self {
            cols,
            rows,
            tile_size,
            tiles: vec![Self::EMPTY; cols * rows],
        }
    }

    /// 列数。
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// 行数。
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// 每格的世界尺寸。
    pub fn tile_size(&self) -> Vec2 {
        self.tile_size
    }

    /// 取一格。越界返回 [`TileMap::EMPTY`]。
    pub fn get(&self, col: usize, row: usize) -> u32 {
        if col >= self.cols || row >= self.rows {
            return Self::EMPTY;
        }
        self.tiles[row * self.cols + col]
    }

    /// 设一格。越界时什么都不做。
    pub fn set(&mut self, col: usize, row: usize, tile: u32) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        self.tiles[row * self.cols + col] = tile;
    }

    /// 整片填同一个瓦片。
    pub fn fill(&mut self, tile: u32) {
        self.tiles.fill(tile);
    }

    /// 非空瓦片的数量。
    pub fn filled_count(&self) -> usize {
        self.tiles.iter().filter(|t| **t != Self::EMPTY).count()
    }

    /// 整张地图的世界尺寸。
    pub fn world_size(&self) -> Vec2 {
        Vec2::new(
            self.cols as f32 * self.tile_size.x,
            self.rows as f32 * self.tile_size.y,
        )
    }

    /// 世界坐标落在哪一格。返回 `None` 表示在地图外。
    ///
    /// 地图铺在 XY 平面上，原点在左下角，+Y 向上——2D 的通行约定。
    pub fn tile_at(&self, position: Vec2) -> Option<(usize, usize)> {
        if position.x < 0.0 || position.y < 0.0 {
            return None;
        }
        let col = (position.x / self.tile_size.x) as usize;
        let row = (position.y / self.tile_size.y) as usize;
        (col < self.cols && row < self.rows).then_some((col, row))
    }

    /// 一格的世界矩形 `(左下, 右上)`。
    pub fn tile_bounds(&self, col: usize, row: usize) -> (Vec2, Vec2) {
        let min = Vec2::new(col as f32 * self.tile_size.x, row as f32 * self.tile_size.y);
        (min, min + self.tile_size)
    }

    /// 把一段范围内的非空瓦片合成一块网格。
    ///
    /// `view` 是要合成的世界矩形 `(左下, 右上)`；传 `None` 表示整张图。
    /// 空瓦片和图集里查不到的瓦片都跳过。
    ///
    /// 只合成可见范围是关键：一张 1000×1000 的地图整片合成会生成
    /// 四百万个顶点，而屏幕上只看得见几百格。
    pub fn build(&self, atlas: &Atlas, view: Option<(Vec2, Vec2)>) -> Mesh {
        let (col_range, row_range) = self.visible_range(view);

        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        for row in row_range {
            for col in col_range.clone() {
                let tile = self.get(col, row);
                // 空格不生成几何。生成的话顶点数是实际需要的好几倍，
                // 而且它们全透明，既浪费带宽又给混合添乱。
                if tile == Self::EMPTY {
                    continue;
                }
                let Some(region) = atlas.region(tile as usize) else {
                    continue;
                };

                let (min, max) = self.tile_bounds(col, row);
                push_quad(&mut vertices, &mut indices, min, max, region);
            }
        }

        Mesh::new(vertices, indices)
    }

    /// 与视野相交的格子范围。
    fn visible_range(
        &self,
        view: Option<(Vec2, Vec2)>,
    ) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let Some((lo, hi)) = view else {
            return (0..self.cols, 0..self.rows);
        };

        let col0 = (lo.x / self.tile_size.x).floor().max(0.0) as usize;
        let row0 = (lo.y / self.tile_size.y).floor().max(0.0) as usize;
        // `ceil` 配右开区间正好覆盖到最后一个与视野相交的格子。
        let col1 = ((hi.x / self.tile_size.x).ceil().max(0.0) as usize).min(self.cols);
        let row1 = ((hi.y / self.tile_size.y).ceil().max(0.0) as usize).min(self.rows);

        (col0..col1.max(col0), row0..row1.max(row0))
    }
}

/// 往顶点/索引里推一个方片。
fn push_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    min: Vec2,
    max: Vec2,
    region: SpriteRegion,
) {
    let base = vertices.len() as u32;
    // 法线朝 +Z：2D 铺在 XY 平面上，光从屏幕外照进来。
    let normal = Vec3::Z;

    // UV 的 V 要翻转：贴图坐标原点在**左上**，而地图原点在左下。
    // 不翻的话每一格都是上下颠倒的，而且因为大多数瓦片上下不对称，
    // 一眼就能看出来。
    for (position, uv) in [
        (Vec2::new(min.x, min.y), [region.min.x, region.max.y]),
        (Vec2::new(max.x, min.y), [region.max.x, region.max.y]),
        (Vec2::new(max.x, max.y), [region.max.x, region.min.y]),
        (Vec2::new(min.x, max.y), [region.min.x, region.min.y]),
    ] {
        vertices.push(Vertex::new(
            Vec3::new(position.x, position.y, 0.0),
            normal,
            uv,
        ));
    }

    // 逆时针为正面（从 +Z 看过去）。反了的话整张地图会被背面剔除。
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4×4 格的图集。
    fn atlas() -> Atlas {
        Atlas::grid(4, 4)
    }

    /// 8×6 的地图，每格 1×1。
    fn map() -> TileMap {
        TileMap::new(8, 6, Vec2::ONE)
    }

    #[test]
    fn a_new_map_is_empty() {
        let map = map();
        assert_eq!(map.filled_count(), 0);
        assert_eq!(map.get(0, 0), TileMap::EMPTY);
    }

    #[test]
    fn tile_zero_is_a_real_tile() {
        // 拿 0 当「空」的话图集的第一格永远画不出来。
        let mut map = map();
        map.set(2, 3, 0);
        assert_eq!(map.get(2, 3), 0);
        assert_eq!(map.filled_count(), 1);
        assert!(!map.build(&atlas(), None).indices().is_empty());
    }

    #[test]
    fn out_of_bounds_access_is_safe() {
        let mut map = map();
        map.set(999, 999, 3);
        assert_eq!(map.get(999, 999), TileMap::EMPTY);
        assert_eq!(map.filled_count(), 0);
    }

    #[test]
    fn empty_tiles_produce_no_geometry() {
        // 给空格也生成方片的话，顶点数是实际需要的好几倍，
        // 而且它们全透明，既浪费带宽又给混合添乱。
        let mut map = map();
        map.set(0, 0, 1);
        map.set(7, 5, 2);

        let mesh = map.build(&atlas(), None);
        assert_eq!(mesh.vertices().len(), 2 * 4, "只有两格非空");
        assert_eq!(mesh.indices().len(), 2 * 6);
    }

    #[test]
    fn a_full_map_produces_one_quad_per_tile() {
        let mut map = map();
        map.fill(0);
        let mesh = map.build(&atlas(), None);
        assert_eq!(mesh.vertices().len(), 8 * 6 * 4);
    }

    #[test]
    fn an_unknown_tile_is_skipped() {
        // 图集里查不到的瓦片跳过而不是 panic：地图数据和图集
        // 可能来自不同的文件，对不上是常见的资源错误。
        let mut map = map();
        map.set(0, 0, 999);
        assert!(map.build(&atlas(), None).indices().is_empty());
    }

    #[test]
    fn quads_land_at_the_right_place() {
        let mut map = TileMap::new(4, 4, Vec2::new(10.0, 10.0));
        map.set(2, 1, 0);
        let mesh = map.build(&atlas(), None);

        let xs: Vec<f32> = mesh.vertices().iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = mesh.vertices().iter().map(|v| v.position[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::MAX, f32::min), 20.0);
        assert_eq!(xs.iter().cloned().fold(f32::MIN, f32::max), 30.0);
        assert_eq!(ys.iter().cloned().fold(f32::MAX, f32::min), 10.0);
        assert_eq!(ys.iter().cloned().fold(f32::MIN, f32::max), 20.0);
    }

    #[test]
    fn winding_faces_the_camera() {
        // 反了的话整张地图会被背面剔除，屏幕上什么都没有，还不报错。
        let mut map = map();
        map.fill(0);
        let mesh = map.build(&atlas(), None);

        let v = mesh.vertices();
        for tri in mesh.indices().chunks(3) {
            let a = Vec3::from_array(v[tri[0] as usize].position);
            let b = Vec3::from_array(v[tri[1] as usize].position);
            let c = Vec3::from_array(v[tri[2] as usize].position);
            assert!((b - a).cross(c - a).z > 0.0, "三角形朝后了");
        }
    }

    #[test]
    fn the_v_axis_is_flipped() {
        // 贴图坐标原点在左上，地图原点在左下。不翻的话每一格都上下颠倒，
        // 而且因为大多数瓦片上下不对称，一眼就能看出来。
        let mut map = TileMap::new(1, 1, Vec2::ONE);
        map.set(0, 0, 0);
        let mesh = map.build(&atlas(), None);

        // 左下角那个顶点（y 最小）该取到区域的**大** V。
        let bottom = mesh
            .vertices()
            .iter()
            .min_by(|a, b| a.position[1].total_cmp(&b.position[1]))
            .unwrap();
        let top = mesh
            .vertices()
            .iter()
            .max_by(|a, b| a.position[1].total_cmp(&b.position[1]))
            .unwrap();
        assert!(
            bottom.uv[1] > top.uv[1],
            "V 没翻转：下边 {} 上边 {}",
            bottom.uv[1],
            top.uv[1]
        );
    }

    #[test]
    fn each_tile_gets_its_own_region() {
        let mut map = TileMap::new(2, 1, Vec2::ONE);
        map.set(0, 0, 0);
        map.set(1, 0, 5);
        let mesh = map.build(&atlas(), None);

        let left_u = mesh.vertices()[0].uv[0];
        let right_u = mesh.vertices()[4].uv[0];
        assert_ne!(left_u, right_u, "两格取了同一个区域");
    }

    #[test]
    fn only_the_visible_range_is_built() {
        // 一张 1000×1000 的地图整片合成会生成四百万个顶点，
        // 而屏幕上只看得见几百格。
        let mut map = TileMap::new(200, 200, Vec2::ONE);
        map.fill(0);

        let full = map.build(&atlas(), None).vertices().len();
        let windowed = map
            .build(
                &atlas(),
                Some((Vec2::new(10.0, 10.0), Vec2::new(20.0, 20.0))),
            )
            .vertices()
            .len();

        assert_eq!(full, 200 * 200 * 4);
        assert_eq!(windowed, 10 * 10 * 4);
    }

    #[test]
    fn the_visible_range_covers_partially_overlapping_tiles() {
        // 算窄了的话屏幕边缘会缺一行格子，而且只在某些相机位置出现。
        let mut map = TileMap::new(20, 20, Vec2::new(10.0, 10.0));
        map.fill(0);

        // 视野从 5 到 25：碰到第 0、1、2 三列。
        let mesh = map.build(&atlas(), Some((Vec2::new(5.0, 5.0), Vec2::new(25.0, 25.0))));
        assert_eq!(mesh.vertices().len(), 3 * 3 * 4);
    }

    #[test]
    fn a_view_outside_the_map_builds_nothing() {
        let mut map = map();
        map.fill(0);
        let mesh = map.build(
            &atlas(),
            Some((Vec2::new(500.0, 500.0), Vec2::new(600.0, 600.0))),
        );
        assert!(mesh.indices().is_empty());
    }

    #[test]
    fn a_negative_view_clamps_to_the_origin() {
        // 相机在地图左下角外面时，范围不夹取会得到一个巨大的 usize。
        let mut map = map();
        map.fill(0);
        let mesh = map.build(
            &atlas(),
            Some((Vec2::new(-100.0, -100.0), Vec2::new(2.0, 2.0))),
        );
        assert_eq!(mesh.vertices().len(), 2 * 2 * 4);
    }

    #[test]
    fn world_positions_map_back_to_tiles() {
        let map = TileMap::new(8, 6, Vec2::new(10.0, 10.0));
        assert_eq!(map.tile_at(Vec2::new(0.0, 0.0)), Some((0, 0)));
        assert_eq!(map.tile_at(Vec2::new(15.0, 25.0)), Some((1, 2)));
        // 右上角边界之外。
        assert_eq!(map.tile_at(Vec2::new(80.0, 10.0)), None);
        assert_eq!(map.tile_at(Vec2::new(-1.0, 10.0)), None);
    }

    #[test]
    fn tile_bounds_tile_without_gaps() {
        let map = TileMap::new(4, 4, Vec2::new(7.0, 3.0));
        let (_, first_max) = map.tile_bounds(0, 0);
        let (second_min, _) = map.tile_bounds(1, 0);
        assert_eq!(first_max.x, second_min.x);
    }

    #[test]
    fn the_world_size_matches_the_grid() {
        let map = TileMap::new(8, 6, Vec2::new(10.0, 5.0));
        assert_eq!(map.world_size(), Vec2::new(80.0, 30.0));
    }
}
