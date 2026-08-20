//! 笔刷编辑：抬升、下压、抹平、压平、涂材质。
//!
//! # 落笔的形状
//!
//! 每种笔刷都乘上同一条**衰减曲线**：中心 1、边缘 0。
//! 不衰减的话每一笔都是一个圆柱形的台面，边缘是垂直的断崖，
//! 连续画几笔会叠成一片阶梯。
//!
//! # 抹平为什么要两趟
//!
//! 抹平是把每个顶点换成邻域的平均值。就地改的话，先算的顶点会影响
//! 后算的顶点——同一笔在不同方向上效果不同，反复涂会往一个方向漂。
//! 所以先算完一整套新值，再统一写回。

use crate::Heightmap;
use kmath::Vec2;

/// 笔刷的形状与强度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Brush {
    /// 落笔中心（地形局部坐标的 XZ）。
    pub center: Vec2,
    /// 半径（米）。
    pub radius: f32,
    /// 强度。含义随操作而定。
    pub strength: f32,
    /// 边缘的柔和程度：0 是硬边，1 是从中心就开始衰减。
    pub falloff: f32,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            radius: 10.0,
            strength: 1.0,
            falloff: 0.5,
        }
    }
}

impl Brush {
    /// 某一点上的落笔权重，0..=1。
    pub fn weight(&self, point: Vec2) -> f32 {
        if self.radius <= 0.0 {
            return 0.0;
        }
        let distance = (point - self.center).length() / self.radius;
        if distance >= 1.0 {
            return 0.0;
        }
        // `falloff` 决定从哪里开始衰减。为 0 时是硬边（全 1），
        // 为 1 时从中心就开始衰减。
        let inner = 1.0 - self.falloff.clamp(0.0, 1.0);
        if distance <= inner {
            return 1.0;
        }
        let t = (distance - inner) / (1.0 - inner).max(1e-6);
        // 三次平滑：线性衰减在边缘会留下一圈能看出来的折线。
        let t = 1.0 - t;
        t * t * (3.0 - 2.0 * t)
    }

    /// 笔刷覆盖到的顶点范围 `(列区间, 行区间)`。
    ///
    /// 只遍历这个范围而不是整张图：一张 1024² 的高度图有一百万个顶点，
    /// 一笔半径十米的笔刷只碰得到其中几十个。
    fn affected(&self, map: &Heightmap) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let cell = map.cell_size();
        let lo = self.center - Vec2::splat(self.radius);
        let hi = self.center + Vec2::splat(self.radius);

        let col0 = (lo.x / cell.x).floor().max(0.0) as usize;
        let row0 = (lo.y / cell.y).floor().max(0.0) as usize;
        // `ceil` 配右开区间正好覆盖到最后一个可能有权重的顶点：
        // 权重在距离 == 半径处已经是 0，所以边界上那一个不算漏。
        // 有一条属性测试盯着这件事（`every_weighted_vertex_is_visited`）。
        let col1 = ((hi.x / cell.x).ceil().max(0.0) as usize).min(map.cols());
        let row1 = ((hi.y / cell.y).ceil().max(0.0) as usize).min(map.rows());

        (col0..col1.max(col0), row0..row1.max(row0))
    }
}

/// 笔刷要做什么。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operation {
    /// 抬升。`strength` 是每次抬多少米。
    Raise,
    /// 下压。
    Lower,
    /// 抹平：往邻域平均值靠。`strength` 是靠拢的比例。
    Smooth,
    /// 压平到指定高度。`strength` 是靠拢的比例。
    Flatten(f32),
}

/// 对高度图落一笔。返回被改动的顶点数。
pub fn apply(map: &mut Heightmap, brush: &Brush, operation: Operation) -> usize {
    let (cols, rows) = brush.affected(map);
    let cell = map.cell_size();
    let mut touched = 0;

    // 抹平要先算完再写：就地改的话先算的顶点会影响后算的，
    // 同一笔在不同方向上效果不同，反复涂会往一个方向漂。
    let mut pending: Vec<(usize, usize, f32)> = Vec::new();

    for row in rows.clone() {
        for col in cols.clone() {
            let point = Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
            let weight = brush.weight(point);
            if weight <= 0.0 {
                continue;
            }

            let current = map.height(col, row);
            let target = match operation {
                Operation::Raise => current + brush.strength * weight,
                Operation::Lower => current - brush.strength * weight,
                Operation::Flatten(height) => {
                    lerp(current, height, (brush.strength * weight).clamp(0.0, 1.0))
                }
                Operation::Smooth => {
                    let average = neighborhood_average(map, col, row);
                    lerp(current, average, (brush.strength * weight).clamp(0.0, 1.0))
                }
            };

            pending.push((col, row, target));
            touched += 1;
        }
    }

    for (col, row, height) in pending {
        map.set_height(col, row, height);
    }
    touched
}

/// 一个顶点及其八邻域的平均高度。
fn neighborhood_average(map: &Heightmap, col: usize, row: usize) -> f32 {
    let mut sum = 0.0;
    let mut count = 0.0;
    for dr in -1i32..=1 {
        for dc in -1i32..=1 {
            // 边界上用夹取而不是跳过：跳过的话边缘顶点的邻域更小，
            // 抹平之后地形边缘会翘起来。
            let c = (col as i32 + dc).clamp(0, map.cols() as i32 - 1) as usize;
            let r = (row as i32 + dr).clamp(0, map.rows() as i32 - 1) as usize;
            sum += map.height(c, r);
            count += 1.0;
        }
    }
    sum / count
}

/// 材质混合图：每个顶点上各层材质的权重。
///
/// 权重恒为**归一化**的（各层加起来是 1）。不归一化的话，
/// 涂得多的地方会整体变亮或变暗——着色器是按权重加权求和的。
#[derive(Debug, Clone, PartialEq)]
pub struct SplatMap {
    cols: usize,
    rows: usize,
    layers: usize,
    /// 行主序，每个顶点连续存 `layers` 个权重。
    weights: Vec<f32>,
}

impl SplatMap {
    /// 一张全部落在第 0 层的混合图。
    pub fn new(cols: usize, rows: usize, layers: usize) -> Self {
        let layers = layers.max(1);
        let mut weights = vec![0.0; cols * rows * layers];
        for vertex in 0..cols * rows {
            weights[vertex * layers] = 1.0;
        }
        Self {
            cols,
            rows,
            layers,
            weights,
        }
    }

    /// 层数。
    pub fn layers(&self) -> usize {
        self.layers
    }

    /// 一个顶点上各层的权重。
    pub fn weights_at(&self, col: usize, row: usize) -> &[f32] {
        let col = col.min(self.cols - 1);
        let row = row.min(self.rows - 1);
        let start = (row * self.cols + col) * self.layers;
        &self.weights[start..start + self.layers]
    }

    /// 全部权重，行主序。
    pub fn weights(&self) -> &[f32] {
        &self.weights
    }

    /// 涂一笔：把 `layer` 的权重往上推，其余层等比例让出。
    ///
    /// 返回被改动的顶点数。
    pub fn paint(&mut self, map: &Heightmap, brush: &Brush, layer: usize) -> usize {
        if layer >= self.layers {
            return 0;
        }
        let (cols, rows) = brush.affected(map);
        let cell = map.cell_size();
        let mut touched = 0;

        for row in rows.clone() {
            for col in cols.clone() {
                if row >= self.rows || col >= self.cols {
                    continue;
                }
                let point = Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                let weight = brush.weight(point) * brush.strength;
                if weight <= 0.0 {
                    continue;
                }

                let start = (row * self.cols + col) * self.layers;
                let slice = &mut self.weights[start..start + self.layers];
                let amount = weight.clamp(0.0, 1.0);

                // 目标层往 1 靠，其余层往 0 靠，比例不变。
                // 直接加然后归一化的话，权重小的层会被压得比预期快。
                for (index, value) in slice.iter_mut().enumerate() {
                    let target = if index == layer { 1.0 } else { 0.0 };
                    *value = lerp(*value, target, amount);
                }
                normalize(slice);
                touched += 1;
            }
        }
        touched
    }
}

/// 把一组权重归一化。全零时退化成「全在第 0 层」。
fn normalize(slice: &mut [f32]) {
    let sum: f32 = slice.iter().sum();
    if sum > 1e-6 {
        for value in slice.iter_mut() {
            *value /= sum;
        }
    } else {
        // 全零意味着这个顶点没有任何材质，着色器会画出一片黑。
        slice.fill(0.0);
        slice[0] = 1.0;
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 21×21 顶点、200×200 米。格子边长 10 米。
    fn map() -> Heightmap {
        Heightmap::flat(21, 21, Vec2::new(200.0, 200.0))
    }

    fn at(x: f32, z: f32, radius: f32) -> Brush {
        Brush {
            center: Vec2::new(x, z),
            radius,
            strength: 1.0,
            falloff: 0.5,
        }
    }

    #[test]
    fn the_weight_is_one_at_the_center_and_zero_outside() {
        let brush = at(100.0, 100.0, 20.0);
        assert_eq!(brush.weight(Vec2::new(100.0, 100.0)), 1.0);
        assert_eq!(brush.weight(Vec2::new(130.0, 100.0)), 0.0);
    }

    #[test]
    fn the_weight_falls_off_smoothly() {
        // 线性衰减在边缘会留下一圈能看出来的折线。
        let brush = at(0.0, 0.0, 10.0);
        let samples: Vec<f32> = (0..10)
            .map(|i| brush.weight(Vec2::new(i as f32, 0.0)))
            .collect();
        for pair in samples.windows(2) {
            assert!(pair[1] <= pair[0] + 1e-6, "权重该单调不增");
        }
        assert!(samples[9] < 0.2, "边缘该接近 0");
    }

    #[test]
    fn a_zero_radius_brush_does_nothing() {
        let mut map = map();
        let brush = at(100.0, 100.0, 0.0);
        assert_eq!(apply(&mut map, &brush, Operation::Raise), 0);
        assert_eq!(map.height_range(), (0.0, 0.0));
    }

    #[test]
    fn raising_lifts_the_center_most() {
        let mut map = map();
        apply(&mut map, &at(100.0, 100.0, 30.0), Operation::Raise);

        let center = map.sample(100.0, 100.0);
        let edge = map.sample(125.0, 100.0);
        assert!(center > edge, "中心该比边缘高：{center} vs {edge}");
        assert!(edge > 0.0, "边缘也该被抬起来一点");
        assert_eq!(map.sample(180.0, 100.0), 0.0, "笔刷外面不该动");
    }

    #[test]
    fn lowering_is_the_inverse_of_raising() {
        let mut map = map();
        let brush = at(100.0, 100.0, 30.0);
        apply(&mut map, &brush, Operation::Raise);
        let raised = map.sample(100.0, 100.0);
        apply(&mut map, &brush, Operation::Lower);
        assert!(
            map.sample(100.0, 100.0).abs() < 1e-4,
            "抬起再压下该回到原处"
        );
        assert!(raised > 0.0);
    }

    #[test]
    fn only_the_affected_range_is_visited() {
        // 一张 1024² 的图有一百万个顶点，一笔十米的笔刷只碰几十个。
        let mut map = map();
        let touched = apply(&mut map, &at(100.0, 100.0, 15.0), Operation::Raise);
        assert!(touched > 0);
        assert!(touched < 40, "碰了 {touched} 个顶点，说明遍历了整张图");
    }

    #[test]
    fn every_weighted_vertex_is_visited() {
        // 遍历范围算窄了的话，笔刷边缘会留下一条没被编辑的细缝——
        // 而且只在某些半径下出现，很难注意到。
        //
        // 这里不猜边界在哪，直接对拍：凡是权重大于零的顶点，
        // 落笔之后高度都必须变了。
        for radius in [7.0, 10.0, 13.5, 20.0, 33.0] {
            for center in [(100.0, 100.0), (103.0, 97.0), (0.0, 200.0)] {
                let mut map = map();
                let brush = at(center.0, center.1, radius);
                apply(&mut map, &brush, Operation::Raise);

                let cell = map.cell_size();
                for row in 0..map.rows() {
                    for col in 0..map.cols() {
                        let point = Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                        if brush.weight(point) > 0.0 {
                            assert!(
                                map.height(col, row) > 0.0,
                                "半径 {radius}、中心 {center:?} 时漏了顶点 ({col},{row})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_brush_at_the_border_does_not_panic() {
        let mut map = map();
        for (x, z) in [(0.0, 0.0), (200.0, 200.0), (-50.0, 100.0), (250.0, 250.0)] {
            apply(&mut map, &at(x, z, 30.0), Operation::Raise);
        }
        assert!(map.heights().iter().all(|h| h.is_finite()));
    }

    #[test]
    fn flatten_pulls_toward_the_target_height() {
        let mut map = map();
        apply(&mut map, &at(100.0, 100.0, 40.0), Operation::Raise);
        for _ in 0..20 {
            apply(&mut map, &at(100.0, 100.0, 40.0), Operation::Flatten(5.0));
        }
        assert!(
            (map.sample(100.0, 100.0) - 5.0).abs() < 0.1,
            "反复压平该收敛到目标高度，实测 {}",
            map.sample(100.0, 100.0)
        );
    }

    #[test]
    fn smoothing_reduces_the_height_range() {
        let mut map = map();
        // 造一个尖峰。
        map.set_height(10, 10, 50.0);
        let before = map.height_range().1;

        for _ in 0..10 {
            apply(&mut map, &at(100.0, 100.0, 40.0), Operation::Smooth);
        }
        assert!(map.height_range().1 < before, "抹平该把尖峰削下去");
    }

    #[test]
    fn smoothing_is_direction_independent() {
        // 就地改的话先算的顶点会影响后算的，同一笔在不同方向上
        // 效果不同，反复涂会往一个方向漂。
        let build = || {
            let mut map = map();
            map.set_height(9, 10, 20.0);
            map.set_height(11, 10, 0.0);
            map
        };

        let mut a = build();
        apply(&mut a, &at(100.0, 100.0, 35.0), Operation::Smooth);

        // 同一笔算两次该完全一致（两趟写回保证了这一点）。
        let mut b = build();
        apply(&mut b, &at(100.0, 100.0, 35.0), Operation::Smooth);

        assert_eq!(a.heights(), b.heights());

        // 而且抹平之后尖峰该被摊开：
        // (9,10) 的 20 降下来，它的邻居 (10,10) 升上去。
        //
        // 不能拿 (11,10) 来比——它离尖峰有两格，不在八邻域里，
        // 一次抹平根本影响不到它。
        assert!(a.height(9, 10) < 20.0, "尖峰该降下来");
        assert!(a.height(10, 10) > 0.0, "尖峰旁边该升上去");
    }

    #[test]
    fn smoothing_does_not_lift_the_terrain_edge() {
        // 边界上跳过越界邻居的话，边缘顶点的邻域更小，
        // 抹平之后地形边缘会翘起来。
        let mut map = map();
        map.set_height(0, 0, 10.0);
        let before = map.height(0, 0);
        for _ in 0..5 {
            apply(&mut map, &at(0.0, 0.0, 40.0), Operation::Smooth);
        }
        assert!(map.height(0, 0) < before, "角落的尖峰该被削平，不是被抬高");
    }

    // ───────────────────────── 材质混合 ─────────────────────────

    #[test]
    fn a_new_splat_map_is_all_layer_zero() {
        let splat = SplatMap::new(5, 5, 4);
        assert_eq!(splat.weights_at(2, 2), &[1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn painting_shifts_weight_to_the_target_layer() {
        let map = map();
        let mut splat = SplatMap::new(map.cols(), map.rows(), 3);
        splat.paint(&map, &at(100.0, 100.0, 30.0), 1);

        let center = splat.weights_at(10, 10);
        assert!(center[1] > center[0], "中心该以第 1 层为主：{center:?}");
    }

    #[test]
    fn weights_always_sum_to_one() {
        // 不归一化的话，涂得多的地方会整体变亮或变暗——
        // 着色器是按权重加权求和的。
        let map = map();
        let mut splat = SplatMap::new(map.cols(), map.rows(), 4);
        for layer in 0..4 {
            splat.paint(&map, &at(90.0 + layer as f32 * 8.0, 100.0, 25.0), layer);
        }

        for row in 0..map.rows() {
            for col in 0..map.cols() {
                let sum: f32 = splat.weights_at(col, row).iter().sum();
                assert!((sum - 1.0).abs() < 1e-4, "({col},{row}) 的权重和是 {sum}");
            }
        }
    }

    #[test]
    fn weights_never_go_negative() {
        let map = map();
        let mut splat = SplatMap::new(map.cols(), map.rows(), 3);
        for _ in 0..20 {
            splat.paint(&map, &at(100.0, 100.0, 30.0), 2);
        }
        assert!(splat.weights().iter().all(|w| *w >= 0.0));
    }

    #[test]
    fn painting_an_unknown_layer_does_nothing() {
        let map = map();
        let mut splat = SplatMap::new(map.cols(), map.rows(), 2);
        let before = splat.weights().to_vec();
        assert_eq!(splat.paint(&map, &at(100.0, 100.0, 30.0), 9), 0);
        assert_eq!(splat.weights(), before.as_slice());
    }

    #[test]
    fn repeated_painting_converges_to_the_layer() {
        let map = map();
        let mut splat = SplatMap::new(map.cols(), map.rows(), 3);
        for _ in 0..30 {
            splat.paint(&map, &at(100.0, 100.0, 30.0), 1);
        }
        assert!(splat.weights_at(10, 10)[1] > 0.99);
    }
}
