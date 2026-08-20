//! 高度图：一张规则网格上的高度值，以及在它上面做的采样与求交。
//!
//! # 坐标约定
//!
//! 高度图铺在 XZ 平面上，Y 是高度。网格是 `cols × rows` 个**顶点**
//! （不是格子），所以 `cols` 列的网格有 `cols - 1` 个格子。
//!
//! 这个「顶点数 vs 格子数」的差一位是高度图里最常见的错误来源：
//! 用格子数去算步长，整块地形会缩掉一格；用顶点数去遍历三角形，
//! 最后一行会越界。所有换算都走 [`Heightmap::cell_size`] 与
//! [`Heightmap::size`]，不在调用点手算。

use kmath::{Vec2, Vec3};

/// 一张高度图。
#[derive(Debug, Clone, PartialEq)]
pub struct Heightmap {
    /// 沿 X 的顶点数。
    cols: usize,
    /// 沿 Z 的顶点数。
    rows: usize,
    /// 行主序的高度值，长度恒为 `cols * rows`。
    heights: Vec<f32>,
    /// 整块地形在 XZ 上的尺寸（米）。
    size: Vec2,
}

impl Heightmap {
    /// 一张全平的高度图。
    ///
    /// `cols` / `rows` 是**顶点数**，至少 2——只有一个顶点的网格
    /// 连一个格子都构不成，后面所有步长计算都会除以零。
    pub fn flat(cols: usize, rows: usize, size: Vec2) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(2);
        Self {
            cols,
            rows,
            heights: vec![0.0; cols * rows],
            size,
        }
    }

    /// 用现成的高度值构造。长度不符时返回 `None`。
    pub fn new(cols: usize, rows: usize, size: Vec2, heights: Vec<f32>) -> Option<Self> {
        if cols < 2 || rows < 2 || heights.len() != cols * rows {
            return None;
        }
        Some(Self {
            cols,
            rows,
            heights,
            size,
        })
    }

    /// 沿 X 的顶点数。
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// 沿 Z 的顶点数。
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// 整块地形在 XZ 上的尺寸。
    pub fn size(&self) -> Vec2 {
        self.size
    }

    /// 一个格子多大。
    ///
    /// 除的是**格子数**（顶点数减一），不是顶点数。用顶点数的话
    /// 整块地形会缩掉一格，而且格子越少偏差越明显。
    pub fn cell_size(&self) -> Vec2 {
        Vec2::new(
            self.size.x / (self.cols - 1) as f32,
            self.size.y / (self.rows - 1) as f32,
        )
    }

    /// 全部高度值，行主序。
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// 取一个顶点的高度。越界时**夹取**而不是返回 `None`。
    ///
    /// 夹取是有意的：法线计算要看邻居，边界上的顶点没有外侧邻居。
    /// 夹取相当于把边界外当成边界的延伸，得到的法线是平的——
    /// 比返回 0 强得多，后者会在地形边缘造出一圈假的悬崖。
    pub fn height(&self, col: usize, row: usize) -> f32 {
        let col = col.min(self.cols - 1);
        let row = row.min(self.rows - 1);
        self.heights[row * self.cols + col]
    }

    /// 改一个顶点的高度。越界时什么都不做。
    pub fn set_height(&mut self, col: usize, row: usize, height: f32) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        self.heights[row * self.cols + col] = height;
    }

    /// 顶点的世界坐标（相对地形原点，原点在左上角 `(0, 0)`）。
    pub fn vertex(&self, col: usize, row: usize) -> Vec3 {
        let cell = self.cell_size();
        Vec3::new(
            col as f32 * cell.x,
            self.height(col, row),
            row as f32 * cell.y,
        )
    }

    /// 任意 XZ 位置的高度，双线性插值。
    ///
    /// 位置在地形外时夹到边界上。
    pub fn sample(&self, x: f32, z: f32) -> f32 {
        let cell = self.cell_size();
        let fx = (x / cell.x).clamp(0.0, (self.cols - 1) as f32);
        let fz = (z / cell.y).clamp(0.0, (self.rows - 1) as f32);

        let (c0, r0) = (fx.floor() as usize, fz.floor() as usize);
        let (c1, r1) = ((c0 + 1).min(self.cols - 1), (r0 + 1).min(self.rows - 1));
        let (tx, tz) = (fx - c0 as f32, fz - r0 as f32);

        let top = lerp(self.height(c0, r0), self.height(c1, r0), tx);
        let bottom = lerp(self.height(c0, r1), self.height(c1, r1), tx);
        lerp(top, bottom, tz)
    }

    /// 任意 XZ 位置的法线。
    ///
    /// 用**中心差分**而不是叉乘相邻三角形：中心差分只要四个采样，
    /// 而且在格子内部是连续的——按三角形算的话，同一个格子的两半
    /// 法线不同，地形上会出现一格一格的明暗块。
    pub fn normal(&self, x: f32, z: f32) -> Vec3 {
        let cell = self.cell_size();
        let left = self.sample(x - cell.x, z);
        let right = self.sample(x + cell.x, z);
        let up = self.sample(x, z - cell.y);
        let down = self.sample(x, z + cell.y);

        // 梯度的负方向就是法线在 XZ 上的分量。
        Vec3::new(
            (left - right) * cell.y,
            2.0 * cell.x * cell.y,
            (up - down) * cell.x,
        )
        .normalize_or(Vec3::Y)
    }

    /// 高度的最小值与最大值。用来算包围盒。
    pub fn height_range(&self) -> (f32, f32) {
        self.heights.iter().fold((f32::MAX, f32::MIN), |(lo, hi), h| {
            (lo.min(*h), hi.max(*h))
        })
    }

    /// 一条射线和地形的交点（地形局部坐标）。
    ///
    /// 用**沿射线步进 + 二分细化**，不是逐三角形求交：
    /// 一块 512×512 的地形有五十万个三角形，逐个判定在编辑器里
    /// 每帧做一次是不可接受的。步进只看高度差的符号变化。
    ///
    /// `max_distance` 之内没交点时返回 `None`。
    pub fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Vec3> {
        let direction = direction.normalize_or_zero();
        if direction == Vec3::ZERO {
            return None;
        }

        // 步长取一个格子：再小是白费，再大会漏掉窄的山脊。
        let step = self.cell_size().min_element().max(0.01);
        let mut travelled = 0.0;
        let mut previous = origin;
        let mut previous_above = origin.y > self.sample(origin.x, origin.z);

        while travelled < max_distance {
            travelled += step;
            let point = origin + direction * travelled;
            let above = point.y > self.sample(point.x, point.z);

            // 符号变了说明这一段跨过了地表。
            if above != previous_above {
                return Some(self.refine(previous, point));
            }
            previous = point;
            previous_above = above;
        }
        None
    }

    /// 在一段已知跨过地表的线段上二分，收敛到交点。
    fn refine(&self, mut a: Vec3, mut b: Vec3) -> Vec3 {
        // 二十次二分把一个格子的误差压到百万分之一，够了。
        for _ in 0..20 {
            let mid = (a + b) * 0.5;
            if (mid.y > self.sample(mid.x, mid.z)) == (a.y > self.sample(a.x, a.z)) {
                a = mid;
            } else {
                b = mid;
            }
        }
        (a + b) * 0.5
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张 5×5、覆盖 40×40 米的平地。格子边长 10 米。
    fn flat() -> Heightmap {
        Heightmap::flat(5, 5, Vec2::new(40.0, 40.0))
    }

    /// 一张沿 X 线性升高的坡：高度 = 列号。
    fn ramp() -> Heightmap {
        let mut map = flat();
        for row in 0..map.rows() {
            for col in 0..map.cols() {
                map.set_height(col, row, col as f32);
            }
        }
        map
    }

    #[test]
    fn cell_size_divides_by_cells_not_vertices() {
        // 用顶点数去除的话整块地形会缩掉一格：40/5 = 8 而不是 10。
        assert_eq!(flat().cell_size(), Vec2::new(10.0, 10.0));
    }

    #[test]
    fn a_degenerate_size_is_rejected() {
        // 只有一个顶点连一个格子都构不成，步长会除以零。
        let map = Heightmap::flat(1, 1, Vec2::new(10.0, 10.0));
        assert!(map.cols() >= 2 && map.rows() >= 2);
        assert!(map.cell_size().x.is_finite());
    }

    #[test]
    fn mismatched_height_data_is_rejected() {
        assert!(Heightmap::new(4, 4, Vec2::ONE, vec![0.0; 15]).is_none());
        assert!(Heightmap::new(4, 4, Vec2::ONE, vec![0.0; 16]).is_some());
    }

    #[test]
    fn sampling_a_vertex_returns_its_height() {
        let map = ramp();
        for col in 0..map.cols() {
            let x = col as f32 * map.cell_size().x;
            assert!((map.sample(x, 0.0) - col as f32).abs() < 1e-5);
        }
    }

    #[test]
    fn sampling_between_vertices_interpolates() {
        let map = ramp();
        // 第 0 列高 0、第 1 列高 1，正中间该是 0.5。
        assert!((map.sample(5.0, 0.0) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn sampling_outside_clamps_to_the_edge() {
        // 返回 0 的话地形边缘会出现一圈假的悬崖。
        let map = ramp();
        assert_eq!(map.sample(-100.0, 0.0), map.sample(0.0, 0.0));
        assert_eq!(map.sample(1000.0, 0.0), map.sample(40.0, 0.0));
    }

    #[test]
    fn a_flat_map_has_upward_normals() {
        let map = flat();
        for (x, z) in [(0.0, 0.0), (17.0, 23.0), (40.0, 40.0)] {
            let n = map.normal(x, z);
            assert!((n - Vec3::Y).length() < 1e-5, "在 ({x},{z}) 处法线是 {n:?}");
        }
    }

    #[test]
    fn a_slope_tilts_the_normal_against_the_gradient() {
        // 沿 +X 升高，法线该往 -X 倒。
        let map = ramp();
        let n = map.normal(20.0, 20.0);
        assert!(n.x < 0.0, "法线该背着上坡方向倒，实测 {n:?}");
        assert!(n.y > 0.0, "法线不该朝下");
        assert!(n.z.abs() < 1e-5, "沿 Z 没有坡度");
    }

    #[test]
    fn normals_are_continuous_inside_a_cell() {
        // 按相邻三角形叉乘的话，同一个格子的两半法线不同，
        // 地形上会出现一格一格的明暗块。
        let mut map = flat();
        map.set_height(2, 2, 5.0);

        let a = map.normal(15.0, 15.0);
        let b = map.normal(15.01, 15.01);
        assert!((a - b).length() < 0.01, "格子内部法线跳变了：{a:?} → {b:?}");
    }

    #[test]
    fn normals_are_unit_length() {
        let mut map = flat();
        map.set_height(1, 1, 12.0);
        map.set_height(3, 2, -7.0);
        for (x, z) in [(0.0, 0.0), (12.0, 8.0), (39.0, 39.0)] {
            assert!((map.normal(x, z).length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn the_height_range_covers_everything() {
        let mut map = flat();
        map.set_height(0, 0, -3.0);
        map.set_height(4, 4, 9.0);
        let (lo, hi) = map.height_range();
        assert_eq!((lo, hi), (-3.0, 9.0));
    }

    #[test]
    fn out_of_bounds_writes_are_ignored() {
        let mut map = flat();
        map.set_height(99, 99, 5.0);
        assert_eq!(map.height_range(), (0.0, 0.0));
    }

    #[test]
    fn reading_out_of_bounds_clamps() {
        // 法线计算要看邻居，边界上的顶点没有外侧邻居。
        let map = ramp();
        assert_eq!(map.height(999, 0), map.height(4, 0));
    }

    #[test]
    fn a_ray_straight_down_hits_the_surface() {
        let mut map = flat();
        map.set_height(2, 2, 10.0);

        let hit = map
            .raycast(Vec3::new(20.0, 100.0, 20.0), Vec3::NEG_Y, 200.0)
            .expect("垂直往下一定打得到");
        assert!((hit.x - 20.0).abs() < 1e-3);
        assert!((hit.z - 20.0).abs() < 1e-3);
        assert!((hit.y - 10.0).abs() < 0.05, "命中高度 {}", hit.y);
    }

    #[test]
    fn a_ray_pointing_away_misses() {
        let map = flat();
        assert!(map.raycast(Vec3::new(20.0, 10.0, 20.0), Vec3::Y, 100.0).is_none());
    }

    #[test]
    fn a_short_ray_misses() {
        // 距离不够时不该「差一点也算命中」。
        let map = flat();
        assert!(
            map.raycast(Vec3::new(20.0, 100.0, 20.0), Vec3::NEG_Y, 10.0)
                .is_none()
        );
    }

    #[test]
    fn a_zero_direction_ray_misses_instead_of_hanging() {
        // 归一化零向量得到 NaN，步进会永远走不到头。
        let map = flat();
        assert!(map.raycast(Vec3::ZERO, Vec3::ZERO, 100.0).is_none());
    }

    #[test]
    fn a_horizontal_ray_hits_where_the_slope_rises_to_meet_it() {
        // 坡高等于列号，列号 = x/10，所以高度 2 在 x = 20 处。
        // 一条 y = 2 的水平射线该正好在那里穿进坡里。
        //
        // 高度要挑在坡的**范围之内**：坡最高只有 4（第 4 列），
        // 射线取 y = 5 的话永远在地表之上，根本不相交。
        let map = ramp();
        let hit = map
            .raycast(Vec3::new(0.0, 2.0, 20.0), Vec3::X, 100.0)
            .expect("水平射线该在坡升到它的高度处相交");

        assert!((hit.x - 20.0).abs() < 0.2, "该在 x=20 附近相交，实测 {}", hit.x);
        assert!((hit.y - 2.0).abs() < 1e-3, "水平射线的高度不该变");
    }

    #[test]
    fn a_ray_below_the_surface_hits_immediately() {
        // 起点已经在地表之下时，第一步就该判出符号变化。
        let map = ramp();
        let hit = map.raycast(Vec3::new(35.0, -5.0, 20.0), Vec3::Y, 100.0);
        assert!(hit.is_some(), "从地下往上打该打中地表");
    }

    #[test]
    fn the_hit_point_is_on_the_surface() {
        let mut map = flat();
        for col in 0..map.cols() {
            for row in 0..map.rows() {
                map.set_height(col, row, ((col + row) as f32 * 0.7).sin() * 4.0);
            }
        }

        for (x, z) in [(5.0, 5.0), (17.0, 31.0), (33.0, 12.0)] {
            let hit = map
                .raycast(Vec3::new(x, 50.0, z), Vec3::NEG_Y, 200.0)
                .expect("垂直往下一定打得到");
            let surface = map.sample(hit.x, hit.z);
            assert!(
                (hit.y - surface).abs() < 0.05,
                "命中点不在地表上：{} vs {surface}",
                hit.y
            );
        }
    }
}
