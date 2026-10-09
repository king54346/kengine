//! 导航寻路：2.5D 格子 + A* + 拉直。
//!
//! 这个 crate 不认识场景，也不认识物理：烘焙时只问一个问题——「这个 (x, z) 往下，地面在多高、朝哪」，
//! 由调用方回答（`kscene` 那一层用物理射线 + 地形回答，见 `Scene::bake_nav_grid`）。
//!
//! ```
//! use knav::{GroundSample, NavGrid, NavGridSettings};
//! use kmath::{Vec2, Vec3};
//!
//! // 20 × 20 米的平地，中间一道墙（x = 0，z 在 -6..6，高 2 米）。
//! let grid = NavGrid::bake(Vec2::splat(-10.0), Vec2::splat(10.0), NavGridSettings::default(), |p| {
//!     let wall = p.x.abs() < 0.3 && p.y.abs() < 6.0;
//!     Some(GroundSample { height: if wall { 2.0 } else { 0.0 }, normal: Vec3::Y })
//! });
//! let path = grid.find_path(Vec3::new(-5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 0.0)).unwrap();
//! // 绕过墙头：中间至少拐一次。
//! assert!(path.len() >= 3);
//! ```
//!
//! # 怎么烘
//!
//! 和 Recast 前半段同一个思路，只是不做多边形：
//!
//! 1. 每格中心采一次地面：没有地面、或者坡度超过 `max_slope` 的格子不能走。
//! 2. 相邻两格高度差不超过 `max_step` 才算连通（台阶走得上去，箱子顶走不上去）。
//! 3. **边缘**：能走、但八个邻居里有不连通的格子。从边缘往里量距离（倒角距离变换），
//!    离边缘不到 `agent_radius` 的格子也不能走——寻路时把代理当成一个点，墙角就不会蹭进去。
//!
//! # 局限
//!
//! - 一层：每个 (x, z) 只有一个高度，桥下面那一层没有。
//! - 格子分辨率决定能过多窄的门：门宽要大于 `2 × agent_radius + cell_size` 才稳。
//! - 烘一次是静态的；会动的障碍物用 [`NavGrid::block`] 临时挡住（会重算边缘距离）。

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use kmath::{Vec2, Vec3};

/// 烘焙参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavGridSettings {
    /// 格子边长（米）。
    pub cell_size: f32,
    /// 代理半径：离墙、离悬崖边至少这么远。
    pub agent_radius: f32,
    /// 相邻格子最大高度差（台阶）。和角色控制器的自动上台阶高度对齐。
    pub max_step: f32,
    /// 最大坡度（弧度）。和角色控制器的 `max_slope_climb_angle` 对齐。
    pub max_slope: f32,
}

impl Default for NavGridSettings {
    fn default() -> Self {
        Self {
            cell_size: 0.25,
            agent_radius: 0.35,
            max_step: 0.3,
            max_slope: 45f32.to_radians(),
        }
    }
}

/// 一次地面采样：高度和法线。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundSample {
    pub height: f32,
    pub normal: Vec3,
}

/// 烘好的导航格子。
#[derive(Clone, Debug)]
pub struct NavGrid {
    settings: NavGridSettings,
    /// 格子 (0, 0) 的最小角（世界 x, z）。
    origin: Vec2,
    width: usize,
    depth: usize,
    /// 每格地面高度；没有地面是 NaN。
    height: Vec<f32>,
    /// 有地面、坡度也合格。
    ground: Vec<bool>,
    /// 用 [`NavGrid::block`] 挡住的。
    blocked: Vec<bool>,
    /// 走过这一格的代价倍数（≥ 1）。
    cost: Vec<f32>,
    /// 到最近边缘的距离（米），能不能走就看它够不够 `agent_radius`。
    clearance: Vec<f32>,
    walkable: Vec<bool>,
}

/// 八个方向：(dx, dz)。前四个是正交方向。
const DIRECTIONS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

impl NavGrid {
    /// 在 `min..max`（世界 x, z）上烘一张格子。`sample(p)` 回答 `(p.x, p.y)` 处往下的地面；`None` 是没有地面。
    pub fn bake(
        min: Vec2,
        max: Vec2,
        settings: NavGridSettings,
        mut sample: impl FnMut(Vec2) -> Option<GroundSample>,
    ) -> Self {
        let cell = settings.cell_size.max(1e-3);
        let size = (max - min).max(Vec2::splat(cell));
        let width = (size.x / cell).ceil() as usize;
        let depth = (size.y / cell).ceil() as usize;
        let count = width * depth;
        let min_normal_y = settings.max_slope.cos() - 1e-4;
        let mut height = vec![f32::NAN; count];
        let mut ground = vec![false; count];
        for z in 0..depth {
            for x in 0..width {
                let center = min + Vec2::new(x as f32 + 0.5, z as f32 + 0.5) * cell;
                if let Some(hit) = sample(center)
                    && hit.height.is_finite()
                {
                    let index = z * width + x;
                    height[index] = hit.height;
                    ground[index] = hit.normal.normalize_or_zero().y >= min_normal_y;
                }
            }
        }
        let mut grid = Self {
            settings: NavGridSettings {
                cell_size: cell,
                ..settings
            },
            origin: min,
            width,
            depth,
            height,
            ground,
            blocked: vec![false; count],
            cost: vec![1.0; count],
            clearance: vec![0.0; count],
            walkable: vec![false; count],
        };
        grid.rebuild();
        grid
    }

    pub fn settings(&self) -> &NavGridSettings {
        &self.settings
    }

    pub fn cell_size(&self) -> f32 {
        self.settings.cell_size
    }

    /// 格子数（x 方向，z 方向）。
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.depth)
    }

    /// 格子 (0, 0) 的最小角（世界 x, z）。
    pub fn origin(&self) -> Vec2 {
        self.origin
    }

    /// 能走的格子数。
    pub fn walkable_count(&self) -> usize {
        self.walkable.iter().filter(|w| **w).count()
    }

    /// 世界坐标落在哪一格；格子外是 `None`。
    pub fn cell_at(&self, position: Vec3) -> Option<(usize, usize)> {
        let local = (Vec2::new(position.x, position.z) - self.origin) / self.settings.cell_size;
        if local.x < 0.0 || local.y < 0.0 {
            return None;
        }
        let (x, z) = (local.x as usize, local.y as usize);
        (x < self.width && z < self.depth).then_some((x, z))
    }

    /// 格子中心，y 是那格的地面高度（没有地面时是 0）。
    pub fn cell_center(&self, x: usize, z: usize) -> Vec3 {
        let xz = self.origin + Vec2::new(x as f32 + 0.5, z as f32 + 0.5) * self.settings.cell_size;
        let height = self
            .height
            .get(z * self.width + x)
            .copied()
            .filter(|h| h.is_finite())
            .unwrap_or(0.0);
        Vec3::new(xz.x, height, xz.y)
    }

    pub fn is_walkable(&self, x: usize, z: usize) -> bool {
        x < self.width && z < self.depth && self.walkable[z * self.width + x]
    }

    /// 这个世界坐标所在的格子能不能走。
    pub fn walkable_at(&self, position: Vec3) -> bool {
        self.cell_at(position)
            .is_some_and(|(x, z)| self.is_walkable(x, z))
    }

    /// 这一格离最近的边缘（墙脚、悬崖边、挡住的区域）多远。
    pub fn clearance(&self, x: usize, z: usize) -> f32 {
        if x < self.width && z < self.depth {
            self.clearance[z * self.width + x]
        } else {
            0.0
        }
    }

    /// 地面高度；格子外或没有地面是 `None`。
    pub fn height_at(&self, position: Vec3) -> Option<f32> {
        let (x, z) = self.cell_at(position)?;
        Some(self.height[z * self.width + x]).filter(|h| h.is_finite())
    }

    /// 把一块矩形（世界 x, z）挡住：会动的障碍、关上的门。重算边缘距离。
    pub fn block(&mut self, min: Vec2, max: Vec2) {
        self.for_cells_in(min, max, |grid, index| grid.blocked[index] = true);
        self.rebuild();
    }

    /// 撤掉 [`block`](Self::block)。
    pub fn unblock(&mut self, min: Vec2, max: Vec2) {
        self.for_cells_in(min, max, |grid, index| grid.blocked[index] = false);
        self.rebuild();
    }

    /// 一块矩形的代价倍数（泥地、浅水：能走，但绕得开就绕）。小于 1 的按 1 算——启发函数按 1 估，
    /// 低于 1 会让 A* 不再给出最短路。
    pub fn set_cost(&mut self, min: Vec2, max: Vec2, cost: f32) {
        let cost = cost.max(1.0);
        self.for_cells_in(min, max, |grid, index| grid.cost[index] = cost);
    }

    fn for_cells_in(&mut self, min: Vec2, max: Vec2, mut f: impl FnMut(&mut Self, usize)) {
        let cell = self.settings.cell_size;
        let lo = ((min - self.origin) / cell).floor().max(Vec2::ZERO);
        let hi = ((max - self.origin) / cell).ceil();
        let (x0, z0) = (lo.x as usize, lo.y as usize);
        let (x1, z1) = (
            (hi.x.max(0.0) as usize).min(self.width),
            (hi.y.max(0.0) as usize).min(self.depth),
        );
        for z in z0..z1 {
            for x in x0..x1 {
                f(self, z * self.width + x);
            }
        }
    }

    fn open(&self, index: usize) -> bool {
        self.ground[index] && !self.blocked[index]
    }

    /// 相邻两格（已知都在格子里）走不走得过去：都能站、高度差不超过台阶。
    fn step_ok(&self, a: usize, b: usize, use_walkable: bool) -> bool {
        let stand = |i: usize| {
            if use_walkable {
                self.walkable[i]
            } else {
                self.open(i)
            }
        };
        stand(a) && stand(b) && (self.height[a] - self.height[b]).abs() <= self.settings.max_step
    }

    fn neighbor(&self, x: usize, z: usize, (dx, dz): (i32, i32)) -> Option<(usize, usize)> {
        let nx = x as i64 + dx as i64;
        let nz = z as i64 + dz as i64;
        (nx >= 0 && nz >= 0 && (nx as usize) < self.width && (nz as usize) < self.depth)
            .then_some((nx as usize, nz as usize))
    }

    /// 能不能从 (x, z) 一步走到 `dir` 那格：对角线还要求两侧的正交格子也过得去（不切墙角）。
    fn can_move(
        &self,
        x: usize,
        z: usize,
        dir: (i32, i32),
        use_walkable: bool,
    ) -> Option<(usize, usize)> {
        let here = z * self.width + x;
        let (nx, nz) = self.neighbor(x, z, dir)?;
        if !self.step_ok(here, nz * self.width + nx, use_walkable) {
            return None;
        }
        if dir.0 != 0 && dir.1 != 0 {
            for side in [(dir.0, 0), (0, dir.1)] {
                let (sx, sz) = self.neighbor(x, z, side)?;
                if !self.step_ok(here, sz * self.width + sx, use_walkable) {
                    return None;
                }
            }
        }
        Some((nx, nz))
    }

    /// 重算边缘距离和能走的格子。
    fn rebuild(&mut self) {
        let cell = self.settings.cell_size;
        let count = self.width * self.depth;
        // 边缘格：能站，但有邻居（含格子外）不连通。墙在半格之外。
        for z in 0..self.depth {
            for x in 0..self.width {
                let index = z * self.width + x;
                self.clearance[index] = if !self.open(index) {
                    0.0
                } else if DIRECTIONS
                    .iter()
                    .any(|&dir| self.can_move(x, z, dir, false).is_none())
                {
                    cell * 0.5
                } else {
                    f32::INFINITY
                };
            }
        }
        // 倒角距离变换：正交一格 = cell，对角 = √2·cell。两遍扫完。
        let diagonal = cell * std::f32::consts::SQRT_2;
        let forward = [
            (-1, 0, cell),
            (0, -1, cell),
            (-1, -1, diagonal),
            (1, -1, diagonal),
        ];
        let backward = [
            (1, 0, cell),
            (0, 1, cell),
            (1, 1, diagonal),
            (-1, 1, diagonal),
        ];
        for (pass, forward_pass) in [(&forward, true), (&backward, false)] {
            for step in 0..count {
                let index = if forward_pass { step } else { count - 1 - step };
                let (x, z) = (index % self.width, index / self.width);
                for &(dx, dz, w) in pass.iter() {
                    if let Some((nx, nz)) = self.neighbor(x, z, (dx, dz)) {
                        let candidate = self.clearance[nz * self.width + nx] + w;
                        if candidate < self.clearance[index] {
                            self.clearance[index] = candidate;
                        }
                    }
                }
            }
        }
        let radius = self.settings.agent_radius;
        for index in 0..count {
            self.walkable[index] = self.open(index) && self.clearance[index] >= radius - 1e-4;
        }
    }

    /// 离 `position` 最近的能走的格子中心，搜索半径 `max_distance`（水平距离）。
    pub fn nearest_walkable(&self, position: Vec3, max_distance: f32) -> Option<Vec3> {
        let cell = self.settings.cell_size;
        let local = (Vec2::new(position.x, position.z) - self.origin) / cell;
        let reach = (max_distance / cell).ceil() as i64 + 1;
        let (cx, cz) = (local.x.floor() as i64, local.y.floor() as i64);
        let mut best: Option<(f32, Vec3)> = None;
        for z in (cz - reach).max(0)..(cz + reach + 1).min(self.depth as i64) {
            for x in (cx - reach).max(0)..(cx + reach + 1).min(self.width as i64) {
                let (x, z) = (x as usize, z as usize);
                if !self.walkable[z * self.width + x] {
                    continue;
                }
                let center = self.cell_center(x, z);
                let distance = Vec2::new(center.x - position.x, center.z - position.z).length();
                if distance <= max_distance + cell && best.is_none_or(|(d, _)| distance < d) {
                    best = Some((distance, center));
                }
            }
        }
        best.map(|(_, center)| center)
    }

    /// 从 `from` 到 `to` 的路径（拉直过的折线，首尾是起点终点）。
    ///
    /// 起点、终点落在不能走的格子上（贴着墙、站在箱子边）时，先挪到附近能走的格子（`agent_radius + 2 格` 以内）。
    /// 走不到时返回 `None`。点的 y 是格子的地面高度。
    pub fn find_path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let snap = self.settings.agent_radius + self.settings.cell_size * 2.0;
        let start = self.snap(from, snap)?;
        let goal = self.snap(to, snap)?;
        let cells = self.find_path_cells(start, goal)?;
        let mut points: Vec<Vec3> = Vec::with_capacity(cells.len() + 1);
        let start_cell = start;
        let goal_cell = goal;
        // 起点、终点用原来的水平位置（只要它就在那一格里），中间用格子中心。
        let endpoint = |p: Vec3, cell: (usize, usize)| {
            let center = self.cell_center(cell.0, cell.1);
            if self.cell_at(p) == Some(cell) {
                Vec3::new(p.x, center.y, p.z)
            } else {
                center
            }
        };
        points.push(endpoint(from, start_cell));
        for &(x, z) in &cells[1..cells.len().saturating_sub(1)] {
            points.push(self.cell_center(x, z));
        }
        points.push(endpoint(to, goal_cell));
        Some(self.smooth(&points))
    }

    fn snap(&self, position: Vec3, max_distance: f32) -> Option<(usize, usize)> {
        if let Some(cell) = self.cell_at(position)
            && self.is_walkable(cell.0, cell.1)
        {
            return Some(cell);
        }
        self.cell_at(self.nearest_walkable(position, max_distance)?)
    }

    /// A*，返回经过的格子（含首尾）。八方向、不切墙角，代价 = 三维距离 × 两格代价的平均。
    pub fn find_path_cells(
        &self,
        start: (usize, usize),
        goal: (usize, usize),
    ) -> Option<Vec<(usize, usize)>> {
        if !self.is_walkable(start.0, start.1) || !self.is_walkable(goal.0, goal.1) {
            return None;
        }
        let count = self.width * self.depth;
        let index = |(x, z): (usize, usize)| z * self.width + x;
        let (start_index, goal_index) = (index(start), index(goal));
        let cell = self.settings.cell_size;
        let heuristic = |i: usize| {
            let (dx, dz) = (
                (i % self.width).abs_diff(goal.0) as f32,
                (i / self.width).abs_diff(goal.1) as f32,
            );
            let (long, short) = if dx > dz { (dx, dz) } else { (dz, dx) };
            (long - short + short * std::f32::consts::SQRT_2) * cell
        };
        let mut g = vec![f32::INFINITY; count];
        let mut came_from = vec![u32::MAX; count];
        let mut closed = vec![false; count];
        let mut open = BinaryHeap::new();
        g[start_index] = 0.0;
        open.push(Open {
            f: heuristic(start_index),
            index: start_index,
        });
        while let Some(Open { index: current, .. }) = open.pop() {
            if current == goal_index {
                let mut path = vec![goal];
                let mut at = current;
                while at != start_index {
                    at = came_from[at] as usize;
                    path.push((at % self.width, at / self.width));
                }
                path.reverse();
                return Some(path);
            }
            if closed[current] {
                continue;
            }
            closed[current] = true;
            let (x, z) = (current % self.width, current / self.width);
            for dir in DIRECTIONS {
                let Some(next) = self.can_move(x, z, dir, true) else {
                    continue;
                };
                let next_index = index(next);
                if closed[next_index] {
                    continue;
                }
                let horizontal = if dir.0 != 0 && dir.1 != 0 {
                    cell * std::f32::consts::SQRT_2
                } else {
                    cell
                };
                let rise = self.height[next_index] - self.height[current];
                let step = (horizontal * horizontal + rise * rise).sqrt()
                    * 0.5
                    * (self.cost[current] + self.cost[next_index]);
                let tentative = g[current] + step;
                if tentative < g[next_index] {
                    g[next_index] = tentative;
                    came_from[next_index] = current as u32;
                    open.push(Open {
                        f: tentative + heuristic(next_index),
                        index: next_index,
                    });
                }
            }
        }
        None
    }

    /// `a` 到 `b` 的直线能不能直接走：沿线经过的格子都能走、相邻的都连通、代价不比两端高。
    pub fn line_of_sight(&self, a: Vec3, b: Vec3) -> bool {
        let (Some(mut current), Some(end)) = (self.cell_at(a), self.cell_at(b)) else {
            return false;
        };
        if !self.is_walkable(current.0, current.1) {
            return false;
        }
        let cost_limit = self.cost[current.1 * self.width + current.0]
            .max(self.cost[end.1 * self.width + end.0])
            + 1e-4;
        let delta = Vec2::new(b.x - a.x, b.z - a.z);
        let steps = (delta.length() / (self.settings.cell_size * 0.25))
            .ceil()
            .max(1.0) as usize;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let Some(next) = self.cell_at(a + Vec3::new(delta.x, 0.0, delta.y) * t) else {
                return false;
            };
            if next == current {
                continue;
            }
            let dir = (
                next.0 as i32 - current.0 as i32,
                next.1 as i32 - current.1 as i32,
            );
            if dir.0.abs() > 1
                || dir.1.abs() > 1
                || self.can_move(current.0, current.1, dir, true).is_none()
            {
                return false;
            }
            if self.cost[next.1 * self.width + next.0] > cost_limit {
                return false;
            }
            current = next;
        }
        true
    }

    /// 拉直：从一个点出发，一直往后找还看得见的点，看不见了就在上一个看得见的点拐弯。
    fn smooth(&self, points: &[Vec3]) -> Vec<Vec3> {
        if points.len() <= 2 {
            return points.to_vec();
        }
        let mut out = vec![points[0]];
        let mut anchor = 0;
        while anchor < points.len() - 1 {
            let mut next = anchor + 1;
            while next + 1 < points.len() && self.line_of_sight(points[anchor], points[next + 1]) {
                next += 1;
            }
            out.push(points[next]);
            anchor = next;
        }
        out
    }
}

/// A* 的开放表元素：按 f 从小到大出堆。
#[derive(Clone, Copy)]
struct Open {
    f: f32,
    index: usize,
}

impl PartialEq for Open {
    fn eq(&self, other: &Self) -> bool {
        self.f == other.f && self.index == other.index
    }
}

impl Eq for Open {}

impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap 是大顶堆，反过来比。
        other
            .f
            .total_cmp(&self.f)
            .then_with(|| other.index.cmp(&self.index))
    }
}

/// 路径总长（水平 + 竖直）。
pub fn path_length(path: &[Vec3]) -> f32 {
    path.windows(2).map(|w| w[0].distance(w[1])).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(height: impl Fn(Vec2) -> Option<f32>) -> NavGrid {
        flat_with(NavGridSettings::default(), height)
    }

    fn flat_with(settings: NavGridSettings, height: impl Fn(Vec2) -> Option<f32>) -> NavGrid {
        NavGrid::bake(Vec2::splat(-10.0), Vec2::splat(10.0), settings, |p| {
            height(p).map(|h| GroundSample {
                height: h,
                normal: Vec3::Y,
            })
        })
    }

    /// 路径每一段都真的走得通。
    fn assert_walkable(grid: &NavGrid, path: &[Vec3]) {
        for pair in path.windows(2) {
            assert!(
                grid.line_of_sight(pair[0], pair[1]),
                "{} → {} 走不通",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn an_open_field_is_a_straight_line() {
        let grid = flat(|_| Some(0.0));
        let from = Vec3::new(-6.0, 0.0, -3.0);
        let to = Vec3::new(5.0, 0.0, 4.0);
        let path = grid.find_path(from, to).unwrap();
        assert_eq!(path.len(), 2, "{path:?}");
        assert!(path[0].distance(from) < 1e-4 && path[1].distance(to) < 1e-4);
    }

    #[test]
    fn a_wall_is_walked_around_through_the_gap() {
        // x = 0 一道墙，只在 z ∈ (2, 3.5) 留一个 1.5 米的口子。
        let grid = flat(|p| {
            Some(if p.x.abs() < 0.25 && !(p.y > 2.0 && p.y < 3.5) {
                3.0
            } else {
                0.0
            })
        });
        let from = Vec3::new(-4.0, 0.0, -4.0);
        let to = Vec3::new(4.0, 0.0, -4.0);
        let path = grid.find_path(from, to).unwrap();
        assert_walkable(&grid, &path);
        // 必须从口子里过。
        let crossing = path
            .windows(2)
            .find(|w| w[0].x < 0.0 && w[1].x >= 0.0)
            .unwrap();
        let t = -crossing[0].x / (crossing[1].x - crossing[0].x);
        let z = crossing[0].z + (crossing[1].z - crossing[0].z) * t;
        assert!(z > 2.0 && z < 3.5, "在 z = {z} 穿墙");
        assert!(path_length(&path) > 8.0 + 2.0, "{}", path_length(&path));
    }

    #[test]
    fn an_enclosed_goal_is_unreachable() {
        // 以 (5, 5) 为中心、半径 2 的一圈墙。
        let grid = flat(|p| {
            let r = (p - Vec2::splat(5.0)).length();
            Some(if (1.8..2.3).contains(&r) { 2.0 } else { 0.0 })
        });
        assert!(
            grid.find_path(Vec3::new(-5.0, 0.0, -5.0), Vec3::new(5.0, 0.0, 5.0))
                .is_none()
        );
        // 圈里面自己走得通。
        assert!(
            grid.find_path(Vec3::new(4.5, 0.0, 5.0), Vec3::new(5.5, 0.0, 5.0))
                .is_some()
        );
    }

    #[test]
    fn a_doorway_narrower_than_the_agent_is_closed() {
        // 口子 0.5 米宽，代理半径 0.35（要 0.7 米以上）。
        let wall = |p: Vec2| {
            Some(if p.x.abs() < 0.25 && !(p.y > 0.0 && p.y < 0.5) {
                3.0
            } else {
                0.0
            })
        };
        // 把四周用墙围住，只留这扇门。
        let boxed = move |p: Vec2| if p.y.abs() > 9.0 { Some(3.0) } else { wall(p) };
        let from = Vec3::new(-4.0, 0.0, 0.25);
        let to = Vec3::new(4.0, 0.0, 0.25);
        assert!(flat(boxed).find_path(from, to).is_none());
        let thin = NavGridSettings {
            agent_radius: 0.1,
            ..NavGridSettings::default()
        };
        let path = flat_with(thin, boxed)
            .find_path(from, to)
            .expect("瘦的代理挤得过去");
        assert_eq!(path.len(), 2, "门正对着，直线过去：{path:?}");
    }

    #[test]
    fn stairs_climb_and_tall_boxes_do_not() {
        // x > 0 往上一级级 0.2 米（每级 1 米深，顶上 1 米）；z > 5 那块是 1.5 米高的平台。
        let grid = flat(|p| {
            if p.y > 5.0 {
                Some(1.5)
            } else if p.x > 0.0 {
                Some(0.2 * (p.x.floor() + 1.0).min(5.0))
            } else {
                Some(0.0)
            }
        });
        let path = grid
            .find_path(Vec3::new(-3.0, 0.0, 0.0), Vec3::new(7.0, 0.0, 0.0))
            .unwrap();
        assert!(
            (path.last().unwrap().y - 1.0).abs() < 1e-4,
            "爬上了台阶顶：{path:?}"
        );
        // 平台比地面高 1.5 米、比台阶顶高 0.5 米：都上不去，但平台上面自己连通。
        assert!(
            grid.find_path(Vec3::new(-3.0, 0.0, 0.0), Vec3::new(-3.0, 1.5, 7.5))
                .is_none()
        );
        assert!(
            grid.find_path(Vec3::new(7.0, 1.0, 0.0), Vec3::new(7.0, 1.5, 7.5))
                .is_none()
        );
        assert!(
            grid.find_path(Vec3::new(-3.0, 1.5, 7.5), Vec3::new(3.0, 1.5, 7.5))
                .is_some()
        );
        // 坎边上离边缘不到代理半径的格子不能站。
        assert!(!grid.walkable_at(Vec3::new(-3.0, 0.0, 4.9)));
        assert!(grid.walkable_at(Vec3::new(-3.0, 0.0, 4.4)));
    }

    #[test]
    fn steep_ground_is_not_walkable() {
        let grid = NavGrid::bake(
            Vec2::splat(-5.0),
            Vec2::splat(5.0),
            NavGridSettings::default(),
            |p| {
                let normal = if p.x > 0.0 {
                    Vec3::new(-0.8, 0.6, 0.0)
                } else {
                    Vec3::Y
                };
                Some(GroundSample {
                    height: 0.0,
                    normal,
                })
            },
        );
        assert!(grid.walkable_at(Vec3::new(-2.0, 0.0, 0.0)));
        assert!(!grid.walkable_at(Vec3::new(2.0, 0.0, 0.0)));
    }

    #[test]
    fn expensive_cells_are_avoided_when_a_detour_is_cheaper() {
        let mut grid = flat(|_| Some(0.0));
        let from = Vec3::new(-5.0, 0.0, 0.0);
        let to = Vec3::new(5.0, 0.0, 0.0);
        // 中间一条 2 米宽、z 方向 4 米长的泥地，代价 10：绕过去只多走一点。
        grid.set_cost(Vec2::new(-1.0, -2.0), Vec2::new(1.0, 2.0), 10.0);
        let path = grid.find_path(from, to).unwrap();
        assert!(path.len() > 2, "{path:?}");
        for point in &path {
            assert!(
                !(point.x.abs() < 1.0 && point.z.abs() < 2.0),
                "踩进了泥地：{point}"
            );
        }
        assert_walkable(&grid, &path);
    }

    #[test]
    fn blocking_a_corridor_reroutes_and_unblocking_restores_it() {
        let mut grid = flat(|_| Some(0.0));
        let from = Vec3::new(-5.0, 0.0, 0.0);
        let to = Vec3::new(5.0, 0.0, 0.0);
        let direct = path_length(&grid.find_path(from, to).unwrap());
        grid.block(Vec2::new(-0.5, -4.0), Vec2::new(0.5, 4.0));
        let detour = grid.find_path(from, to).unwrap();
        assert!(path_length(&detour) > direct + 1.0);
        assert_walkable(&grid, &detour);
        grid.unblock(Vec2::new(-0.5, -4.0), Vec2::new(0.5, 4.0));
        assert!((path_length(&grid.find_path(from, to).unwrap()) - direct).abs() < 1e-3);
    }

    #[test]
    fn a_goal_inside_a_wall_snaps_to_the_nearest_floor() {
        let grid = flat(|p| Some(if p.x.abs() < 0.25 { 3.0 } else { 0.0 }));
        // 终点贴着墙站（离墙 0.1 米，比代理半径近）。
        let path = grid
            .find_path(Vec3::new(-4.0, 0.0, 0.0), Vec3::new(-0.35, 0.0, 0.0))
            .unwrap();
        let end = *path.last().unwrap();
        assert!(end.x < -0.25 - 0.35 + 1e-3 && end.x > -1.0, "{end}");
    }
}
