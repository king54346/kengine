//! 聚簇前向着色的划分与分配。**纯 CPU，不碰 GPU**。
//!
//! # 要解决的问题
//!
//! 前向渲染里每个片元都要遍历一遍光源数组。十几盏灯还好，几百盏就等于
//! 每个像素做几百次距离计算——而其中绝大多数光源离这个像素十万八千里。
//!
//! 聚簇的办法是：把视锥切成一堆小格子（簇），**每帧算清楚每个簇里有哪几盏
//! 灯**，着色时只遍历自己那个簇的名单。
//!
//! ```text
//! 屏幕切 16×9 块，深度方向切 24 片  →  3456 个簇
//! 每个簇一份光源名单               →  片元只看自己簇里那几盏
//! ```
//!
//! # 深度方向为什么按指数切
//!
//! 均匀切的话，近处几片挤在一起、远处一片横跨几十米——而近处才是光源
//! 密度最高、最需要分得细的地方。指数划分让每片在**屏幕上**的投影大小
//! 大致相等，这是 Avalanche 那篇 2016 年的做法，也是现在的标准解。
//!
//! ```text
//! slice = floor(log(z / near) / log(far / near) * SLICES)
//! ```
//!
//! # 为什么在 CPU 上分配
//!
//! 用计算着色器分配更快，但要多一条依赖链（分配 → 屏障 → 着色），
//! 而且调试起来只能靠读回缓冲。CPU 这边：几百盏灯 × 各自覆盖的几十个簇
//! 是几万次写入，实测在噪声里。**先做对，快是后面的事**——
//! 而且这一层是纯函数，每条规则都能直接写成测试。
//!
//! # 方向光和半球光不进簇
//!
//! 它们没有位置也没有范围，照亮所有东西。塞进簇里等于每个簇都有它们，
//! 白白占名单。所以光源数组分成两段：**前面是全局光，后面是可聚簇的**，
//! 着色器无条件遍历前一段、按簇遍历后一段。

use kmath::{Mat4, Vec3, Vec4};

/// 一个簇最多记多少盏灯。
///
/// 超出的会被丢掉并计数（[`Assignment::overflow`]）。给一个上限而不是
/// 让名单无限长，是因为**名单本身要传上显存**：不封顶的话一个病态场景
/// （几百盏灯堆在同一个角落）能把缓冲撑到几十兆。
///
/// 256 对真实场景够用：一个像素同时被 256 盏灯照到，那画面本来就已经
/// 白成一片了。
pub const MAX_LIGHTS_PER_CLUSTER: usize = 256;

/// 视锥怎么切。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterGrid {
    /// 横向切几块。
    pub tiles_x: u32,
    /// 纵向切几块。
    pub tiles_y: u32,
    /// 深度方向切几片。
    pub slices: u32,
    /// 近平面（正数，视空间距离）。
    pub near: f32,
    /// 远平面。
    pub far: f32,
}

impl Default for ClusterGrid {
    fn default() -> Self {
        Self {
            // 16×9 贴合常见的 16:9 屏幕，每块大约 120×80 像素。
            // 切得更细能让名单更短，但簇的总数是三个维度相乘，
            // 涨得很快——32×18×24 就是 13824 个簇。
            tiles_x: 16,
            tiles_y: 9,
            slices: 24,
            near: 0.1,
            far: 200.0,
        }
    }
}

impl ClusterGrid {
    /// 一共多少个簇。
    pub fn count(&self) -> usize {
        (self.tiles_x * self.tiles_y * self.slices) as usize
    }

    /// 视空间深度（正数）落在第几片。
    ///
    /// 近平面以内夹到 0，远平面以外夹到最后一片——**夹而不是丢**：
    /// 丢掉的话紧贴近平面的那一层会没有光。
    pub fn slice_of(&self, view_depth: f32) -> u32 {
        if self.slices == 0 {
            return 0;
        }
        let near = self.near.max(1e-4);
        let far = self.far.max(near * 1.0001);
        let depth = view_depth.max(near);

        let ratio = (depth / near).ln() / (far / near).ln();
        // `as u32` 对负数是饱和到 0 的，但先夹一遍读起来更清楚。
        (ratio * self.slices as f32).clamp(0.0, (self.slices - 1) as f32) as u32
    }

    /// 第 `slice` 片覆盖的视空间深度范围。
    ///
    /// 和 [`slice_of`](Self::slice_of) 互为反函数，测试拿它对拍。
    pub fn slice_range(&self, slice: u32) -> (f32, f32) {
        let near = self.near.max(1e-4);
        let far = self.far.max(near * 1.0001);
        let ratio = far / near;
        let at = |index: u32| near * ratio.powf(index as f32 / self.slices as f32);
        (at(slice), at(slice + 1))
    }

    /// 三维下标 → 一维下标。
    ///
    /// x 变化最快：着色时同一行相邻的像素多半落在相邻的簇里，
    /// 这样它们读到的名单在内存上也相邻。
    pub fn index(&self, x: u32, y: u32, slice: u32) -> usize {
        let x = x.min(self.tiles_x.saturating_sub(1));
        let y = y.min(self.tiles_y.saturating_sub(1));
        let slice = slice.min(self.slices.saturating_sub(1));
        ((slice * self.tiles_y + y) * self.tiles_x + x) as usize
    }
}

/// 一盏参与聚簇的光源：世界坐标里的一个球。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterLight {
    /// 世界坐标。
    pub position: Vec3,
    /// 作用半径。超出这个距离强度为 0。
    pub radius: f32,
}

/// 分配的结果，可以直接摊平传上显存。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Assignment {
    /// 每个簇一项：`(名单起点, 名单长度)`。长度与 [`ClusterGrid::count`] 一致。
    pub ranges: Vec<[u32; 2]>,
    /// 所有簇的名单首尾相接。存的是光源在**可聚簇那一段**里的下标。
    pub indices: Vec<u32>,
    /// 因为超过 [`MAX_LIGHTS_PER_CLUSTER`] 而被丢掉的条目数。
    ///
    /// 不为 0 说明画面上某些地方少了几盏灯的贡献。调用方可以据此告警——
    /// 静默丢掉的话表现为「某个角落莫名偏暗」，很难查。
    pub overflow: u32,
}

impl Assignment {
    /// 查一个簇的名单。
    pub fn cluster(&self, index: usize) -> &[u32] {
        let Some(&[start, count]) = self.ranges.get(index) else {
            return &[];
        };
        let start = start as usize;
        let end = (start + count as usize).min(self.indices.len());
        &self.indices[start.min(end)..end]
    }
}

/// 把一批光源分配到各个簇里。
///
/// `view` 是世界 → 视空间，`projection` 是视空间 → 裁剪空间。
/// 两者分开传而不是给一个 `view_proj`：**深度切片要的是视空间深度**，
/// 而那在合并之后的矩阵里取不出来。
///
/// 光源在世界空间给，函数内部转到视空间——调用方通常手上就是世界坐标，
/// 让它自己转一遍只会多一处可能转错的地方。
pub fn assign(
    grid: &ClusterGrid,
    lights: &[ClusterLight],
    view: Mat4,
    projection: Mat4,
) -> Assignment {
    let cluster_count = grid.count();
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); cluster_count];
    let mut overflow = 0u32;

    for (light_index, light) in lights.iter().enumerate() {
        if light.radius <= 0.0 {
            continue;
        }
        let center = view.transform_point3(light.position);
        // 右手系的视空间朝 -Z 看，所以「深度」是 -z。
        let depth = -center.z;

        // 整个球都在近平面后面 → 屏幕上看不见。
        if depth + light.radius <= grid.near {
            continue;
        }
        // 整个球都在远平面外面。
        if depth - light.radius >= grid.far {
            continue;
        }

        let (min_slice, max_slice) = (
            grid.slice_of(depth - light.radius),
            grid.slice_of(depth + light.radius),
        );
        let (min_tile, max_tile) = screen_tiles(grid, projection, center, light.radius);

        for slice in min_slice..=max_slice {
            for y in min_tile[1]..=max_tile[1] {
                for x in min_tile[0]..=max_tile[0] {
                    let bucket = &mut buckets[grid.index(x, y, slice)];
                    if bucket.len() >= MAX_LIGHTS_PER_CLUSTER {
                        overflow += 1;
                        continue;
                    }
                    bucket.push(light_index as u32);
                }
            }
        }
    }

    // 摊平。名单首尾相接，每个簇记自己的起点和长度。
    let mut ranges = Vec::with_capacity(cluster_count);
    let mut indices = Vec::new();
    for bucket in &buckets {
        ranges.push([indices.len() as u32, bucket.len() as u32]);
        indices.extend_from_slice(bucket);
    }

    Assignment {
        ranges,
        indices,
        overflow,
    }
}

/// 一个视空间的球在屏幕上覆盖哪些块。返回 `([min_x, min_y], [max_x, max_y])`。
///
/// # 为什么是包围盒而不是精确求交
///
/// 精确的「球 vs 簇的视锥楔形」求交要对每个候选簇算一次，而候选簇本来就是
/// 靠包围盒圈出来的——多出来的那点精度换不回它的代价。
///
/// 保守多算几个簇只会让那几个簇的着色多循环几盏灯，**不会画错**；
/// 少算才会漏光。所以这里一律往大了取。
///
/// # 为什么还要再往外扩一格
///
/// CPU 这边从**光源的 NDC 包围盒**算块号，着色器那边从**片元的像素坐标**
/// 算块号——两条完全不同的算路。片元正好落在块的边界上时，两边可能
/// 各走一边：实测某块 GPU 上 `960 * 16 / 1920` 算出的是 **7.9999995**
/// （驱动把它重排成了乘以倒数），取整成 7，而 CPU 得 8。
///
/// 这不是「写得不一样」，是浮点重排本来就不受源码控制，换块显卡结论
/// 可能就变了。所以**不去赌位级一致**，而是让正确性不依赖它：
/// 往外扩一格，边界上两边选哪边都能取到这盏灯。
///
/// 代价是每盏灯多占几个簇的名单——那只让那几个簇多循环几盏照不到的灯，
/// 而 `light_sample_direction` 对它们会立刻返回衰减 0。
fn screen_tiles(grid: &ClusterGrid, projection: Mat4, center: Vec3, radius: f32) -> ([u32; 2], [u32; 2]) {
    let full = ([0, 0], [grid.tiles_x.saturating_sub(1), grid.tiles_y.saturating_sub(1)]);

    // 球跨过近平面时，它在屏幕上的投影会翻到无穷远——这时老实地
    // 取整屏。不特判的话投影出来的 NDC 是发散的，包围盒会变成 NaN，
    // 而 NaN 参与比较全是 false，结果是**一个簇都不分配**：那盏灯凭空消失。
    if -center.z - radius <= grid.near {
        return full;
    }

    let mut min = [f32::MAX; 2];
    let mut max = [f32::MIN; 2];
    for corner in 0..8 {
        let offset = Vec3::new(
            if corner & 1 == 0 { -radius } else { radius },
            if corner & 2 == 0 { -radius } else { radius },
            if corner & 4 == 0 { -radius } else { radius },
        );
        let point = center + offset;
        let clip: Vec4 = projection * point.extend(1.0);
        if clip.w <= 1e-6 {
            return full;
        }
        let ndc = [clip.x / clip.w, clip.y / clip.w];
        min[0] = min[0].min(ndc[0]);
        min[1] = min[1].min(ndc[1]);
        max[0] = max[0].max(ndc[0]);
        max[1] = max[1].max(ndc[1]);
    }

    // NDC 的 [-1,1] 映到块下标。y 要翻：NDC 的 +y 朝上，而块是从上往下数的。
    let to_tile = |value: f32, count: u32| -> u32 {
        let normalized = (value * 0.5 + 0.5).clamp(0.0, 1.0);
        ((normalized * count as f32) as u32).min(count.saturating_sub(1))
    };

    // 往外扩一格，理由见上面的文档。
    let grow_low = |value: u32| value.saturating_sub(1);
    let grow_high = |value: u32, count: u32| (value + 1).min(count.saturating_sub(1));

    (
        [
            grow_low(to_tile(min[0], grid.tiles_x)),
            grow_low(to_tile(-max[1], grid.tiles_y)),
        ],
        [
            grow_high(to_tile(max[0], grid.tiles_x), grid.tiles_x),
            grow_high(to_tile(-min[1], grid.tiles_y), grid.tiles_y),
        ],
    )
}

#[cfg(test)]
mod test {
    use super::*;

    fn grid() -> ClusterGrid {
        ClusterGrid {
            tiles_x: 16,
            tiles_y: 9,
            slices: 24,
            near: 0.1,
            far: 100.0,
        }
    }

    /// 相机在原点朝 -Z 看。
    fn view() -> Mat4 {
        Mat4::look_at_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y)
    }

    fn projection() -> Mat4 {
        Mat4::perspective_rh(60_f32.to_radians(), 16.0 / 9.0, 0.1, 100.0)
    }

    // ── 划分 ──

    #[test]
    fn the_cluster_count_is_the_product_of_the_three_axes() {
        assert_eq!(grid().count(), 16 * 9 * 24);
    }

    #[test]
    fn slices_get_thicker_with_distance() {
        // 指数划分的全部意义：近处切得细、远处切得粗。
        // 均匀切的话近处几片挤在一起，而那里才是光源密度最高的地方。
        let grid = grid();
        let (near_start, near_end) = grid.slice_range(0);
        let (far_start, far_end) = grid.slice_range(grid.slices - 1);

        assert!(near_end - near_start < far_end - far_start);
        assert!((near_start - grid.near).abs() < 1e-5, "第一片该贴着近平面");
        assert!((far_end - grid.far).abs() < 1e-3, "最后一片该贴着远平面");
    }

    #[test]
    fn slice_of_and_slice_range_agree() {
        // 两个方向必须自洽。不洽的话着色器算出来的簇和 CPU 分配的簇
        // 不是同一个——表现为光照整体错层，而且不报任何错。
        let grid = grid();
        for slice in 0..grid.slices {
            let (start, end) = grid.slice_range(slice);
            let middle = (start + end) * 0.5;
            assert_eq!(
                grid.slice_of(middle),
                slice,
                "第 {slice} 片的中点 {middle} 落回了别的片"
            );
        }
    }

    #[test]
    fn depths_outside_the_range_are_clamped_not_dropped() {
        // 夹而不是丢：丢掉的话紧贴近平面的那一层会没有光。
        let grid = grid();
        assert_eq!(grid.slice_of(-5.0), 0);
        assert_eq!(grid.slice_of(0.0), 0);
        assert_eq!(grid.slice_of(1e9), grid.slices - 1);
    }

    #[test]
    fn a_degenerate_grid_does_not_divide_by_zero() {
        // 配置可能从别处算出来。NaN 的下标会让整块画面失去光照。
        let degenerate = ClusterGrid {
            tiles_x: 0,
            tiles_y: 0,
            slices: 0,
            near: 0.0,
            far: 0.0,
        };
        assert_eq!(degenerate.slice_of(1.0), 0);
        assert_eq!(degenerate.index(5, 5, 5), 0);
    }

    // ── 分配 ──

    #[test]
    fn a_light_lands_in_the_cluster_it_sits_in() {
        // 最基本的一条：正前方 10 米处一盏小灯，只该落在中间那几块、
        // 以及它自己那一片深度上。
        let grid = grid();
        let light = ClusterLight {
            position: Vec3::new(0.0, 0.0, -10.0),
            radius: 0.5,
        };
        let assignment = assign(&grid, &[light], view(), projection());

        let slice = grid.slice_of(10.0);
        let center = grid.index(grid.tiles_x / 2, grid.tiles_y / 2, slice);
        assert_eq!(assignment.cluster(center), &[0], "灯该在正中那个簇里");

        // 而角落那个簇不该有它。
        let corner = grid.index(0, 0, slice);
        assert!(assignment.cluster(corner).is_empty(), "灯不该跑到角落去");
    }

    #[test]
    fn a_light_behind_the_camera_is_skipped_entirely() {
        let grid = grid();
        let light = ClusterLight {
            position: Vec3::new(0.0, 0.0, 5.0),
            radius: 1.0,
        };
        let assignment = assign(&grid, &[light], view(), projection());

        assert!(assignment.indices.is_empty());
    }

    #[test]
    fn a_light_past_the_far_plane_is_skipped() {
        let grid = grid();
        let light = ClusterLight {
            position: Vec3::new(0.0, 0.0, -500.0),
            radius: 1.0,
        };
        assert!(assign(&grid, &[light], view(), projection())
            .indices
            .is_empty());
    }

    #[test]
    fn a_light_straddling_the_near_plane_covers_the_whole_screen() {
        // 球跨过近平面时它的投影会翻到无穷远。不特判的话包围盒是 NaN，
        // 而 NaN 参与比较全是 false——结果是一个簇都不分配，那盏灯凭空消失。
        let grid = grid();
        let light = ClusterLight {
            position: Vec3::new(0.0, 0.0, -0.05),
            radius: 2.0,
        };
        let assignment = assign(&grid, &[light], view(), projection());

        // 至少最近那一片的每个块上都该有它。
        for y in 0..grid.tiles_y {
            for x in 0..grid.tiles_x {
                assert_eq!(
                    assignment.cluster(grid.index(x, y, 0)),
                    &[0],
                    "块 ({x},{y}) 漏了那盏跨近平面的灯"
                );
            }
        }
    }

    #[test]
    fn a_zero_radius_light_is_skipped() {
        // 半径为 0 的灯照不到任何东西，塞进名单只是白占位置。
        let grid = grid();
        let light = ClusterLight {
            position: Vec3::new(0.0, 0.0, -10.0),
            radius: 0.0,
        };
        assert!(assign(&grid, &[light], view(), projection())
            .indices
            .is_empty());
    }

    #[test]
    fn a_bigger_light_covers_more_clusters() {
        // 单调性。半径变大反而覆盖变少的话，说明包围盒算反了。
        let grid = grid();
        let count = |radius: f32| {
            assign(
                &grid,
                &[ClusterLight {
                    position: Vec3::new(0.0, 0.0, -10.0),
                    radius,
                }],
                view(),
                projection(),
            )
            .indices
            .len()
        };

        assert!(count(5.0) > count(1.0));
        assert!(count(1.0) > count(0.2));
    }

    #[test]
    fn every_cluster_range_points_inside_the_index_list() {
        // 起点或长度算错的话着色器会读到别的簇的名单——
        // 表现为光照在屏幕上错位一块，而且不越界不报错。
        let grid = grid();
        let lights: Vec<ClusterLight> = (0..40)
            .map(|i| ClusterLight {
                position: Vec3::new(
                    (i as f32 * 0.7).sin() * 8.0,
                    (i as f32 * 1.1).cos() * 4.0,
                    -3.0 - i as f32 * 1.5,
                ),
                radius: 2.0,
            })
            .collect();
        let assignment = assign(&grid, &lights, view(), projection());

        assert_eq!(assignment.ranges.len(), grid.count());
        for (index, &[start, count]) in assignment.ranges.iter().enumerate() {
            assert!(
                start as usize + count as usize <= assignment.indices.len(),
                "第 {index} 个簇的区间越界了"
            );
            for &light in assignment.cluster(index) {
                assert!((light as usize) < lights.len(), "名单里有不存在的光源");
            }
        }
    }

    #[test]
    fn ranges_are_contiguous_and_in_order() {
        // 名单是首尾相接的。有空洞的话摊平之后的总长和各簇长度之和对不上，
        // 而那个错只在某些簇上表现出来。
        let grid = ClusterGrid {
            tiles_x: 4,
            tiles_y: 4,
            slices: 4,
            ..grid()
        };
        let lights: Vec<ClusterLight> = (0..12)
            .map(|i| ClusterLight {
                position: Vec3::new(i as f32 - 6.0, 0.0, -5.0 - i as f32),
                radius: 1.5,
            })
            .collect();
        let assignment = assign(&grid, &lights, view(), projection());

        let mut cursor = 0u32;
        for &[start, count] in &assignment.ranges {
            assert_eq!(start, cursor, "名单不连续");
            cursor += count;
        }
        assert_eq!(cursor as usize, assignment.indices.len());
    }

    #[test]
    fn overflowing_a_cluster_is_counted_not_silent() {
        // 静默丢掉的话表现为「某个角落莫名偏暗」，很难查。
        let grid = ClusterGrid {
            tiles_x: 1,
            tiles_y: 1,
            slices: 1,
            ..grid()
        };
        let lights: Vec<ClusterLight> = (0..MAX_LIGHTS_PER_CLUSTER + 10)
            .map(|_| ClusterLight {
                position: Vec3::new(0.0, 0.0, -10.0),
                radius: 50.0,
            })
            .collect();
        let assignment = assign(&grid, &lights, view(), projection());

        assert_eq!(assignment.cluster(0).len(), MAX_LIGHTS_PER_CLUSTER);
        assert_eq!(assignment.overflow, 10);
    }

    #[test]
    fn an_empty_scene_still_produces_one_range_per_cluster() {
        // 着色器按簇下标直接取 `ranges[i]`，少一项就是越界。
        let grid = grid();
        let assignment = assign(&grid, &[], view(), projection());

        assert_eq!(assignment.ranges.len(), grid.count());
        assert!(assignment.indices.is_empty());
        assert!(assignment.cluster(grid.count() - 1).is_empty());
    }

    #[test]
    fn an_out_of_range_cluster_query_returns_empty() {
        let assignment = assign(&grid(), &[], view(), projection());
        assert!(assignment.cluster(usize::MAX).is_empty());
    }

    #[test]
    fn lights_off_to_the_side_do_not_reach_the_other_side() {
        // 真正要验的：分配确实**收窄**了名单。全屏都分配的话
        // 聚簇就一点意义都没有，而那种「退化成全分配」的实现照样能跑。
        let grid = grid();
        let left = ClusterLight {
            position: Vec3::new(-6.0, 0.0, -10.0),
            radius: 1.0,
        };
        let assignment = assign(&grid, &[left], view(), projection());

        let slice = grid.slice_of(10.0);
        let right_edge = grid.index(grid.tiles_x - 1, grid.tiles_y / 2, slice);
        assert!(
            assignment.cluster(right_edge).is_empty(),
            "左边的灯跑到屏幕右边去了"
        );
    }
}
