//! 层次包围盒（BVH）——空间划分与批量剔除的加速结构。
//!
//! 用二叉树把一堆包围盒组织起来：父节点的包围盒包住两个子节点，
//! 查询时整棵子树落在视野外就一次跳过，落在视野内就整棵接受，
//! 只有跨在边界上的分支才需要往下走。对万级对象的场景，
//! 这把「逐个判定」的 O(n) 变成接近 O(可见数 + log n)。
//!
//! 选 BVH 而非八叉树：八叉树按空间均匀切分，物体尺寸悬殊时（比如一块大地面
//! 和一堆小石头）会大量跨格；BVH 按物体本身的分布切分，对这种场景更稳。
//!
//! ```
//! use kmath::{Aabb, Bvh, Intersection, Vec3};
//!
//! // 三个沿 x 轴排开的单位盒。
//! let bounds: Vec<Aabb> = (0..3)
//!     .map(|i| Aabb::from_center_half_extents(Vec3::new(i as f32 * 4.0, 0.0, 0.0), Vec3::splat(0.5)))
//!     .collect();
//! let bvh = Bvh::build(&bounds);
//!
//! // 查询体：只罩住最左边那个盒子。
//! let query = Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0));
//! let mut hits = Vec::new();
//! bvh.query(|aabb| aabb.classify_against(&query), &mut hits);
//!
//! assert_eq!(hits, vec![0]);
//! ```

use crate::{Aabb, Intersection, Vec3};

/// 叶节点最多容纳的图元数。再少下去，树本身的遍历开销就盖过收益了。
const MAX_LEAF_SIZE: usize = 4;
/// SAH 判定后仍允许留在一个叶子里的图元数上限。
const MAX_PRIMS_IN_NODE: usize = 16;
/// SAH 分箱数。12 是质量与构建耗时的常见折中。
const BIN_COUNT: usize = 12;
/// 一次遍历相对一次图元判定的代价。低于 1 表示「往下走比逐个判定便宜」。
const TRAVERSAL_COST: f32 = 0.125;

/// 树上的一个节点。
///
/// 内部节点与叶节点共用一份布局：`count` 为 0 即内部节点。
/// 左右子节点在数组里永远相邻，所以只存左子节点下标就够。
#[derive(Debug, Clone, Copy)]
struct Node {
    aabb: Aabb,
    /// 叶节点：图元在 `primitives` 中的起始下标；内部节点：左子节点下标。
    offset: u32,
    /// 叶节点的图元数量；0 表示内部节点。
    count: u32,
}

impl Node {
    fn is_leaf(&self) -> bool {
        self.count > 0
    }
}

/// 一棵静态 BVH。
///
/// [`build`](Bvh::build) 输入一组包围盒，查询结果是这组包围盒的**下标**——
/// BVH 不关心图元究竟是什么，调用方拿下标回自己的数组里取。
#[derive(Debug, Clone, Default)]
pub struct Bvh {
    nodes: Vec<Node>,
    /// 图元下标，按叶节点分段连续排列。
    primitives: Vec<u32>,
    /// 各图元的包围盒，下标即构建时的输入顺序。
    ///
    /// 自己存一份是为了让叶节点内部也能逐个判定：只判定到叶子就整批接受的话，
    /// 结果会是个超集，剔除统计便不再等于逐个判定的结果。
    bounds: Vec<Aabb>,
}

impl Bvh {
    /// 用分箱 SAH（表面积启发式）构建。
    ///
    /// 空包围盒不会导致构建失败：它的质心退化到原点，只是会让所在分支变松。
    pub fn build(bounds: &[Aabb]) -> Self {
        if bounds.is_empty() {
            return Self::default();
        }

        let centroids: Vec<Vec3> = bounds.iter().map(Aabb::center).collect();
        let mut primitives: Vec<u32> = (0..bounds.len() as u32).collect();
        let mut nodes = Vec::with_capacity(bounds.len() * 2);
        nodes.push(Node {
            aabb: Aabb::EMPTY,
            offset: 0,
            count: 0,
        });

        // 显式栈而非递归：万级对象下递归深度虽然只有 log 级，
        // 但退化分布仍可能把栈捅穿。
        let mut stack = vec![(0usize, 0usize, primitives.len())];
        while let Some((node_index, start, end)) = stack.pop() {
            let aabb = primitives[start..end]
                .iter()
                .fold(Aabb::EMPTY, |acc, &p| acc.union(&bounds[p as usize]));
            nodes[node_index].aabb = aabb;

            let count = end - start;
            let split = if count <= MAX_LEAF_SIZE {
                None
            } else {
                find_split(&mut primitives[start..end], &centroids, &aabb, count)
            };

            let Some(offset) = split else {
                nodes[node_index].offset = start as u32;
                nodes[node_index].count = count as u32;
                continue;
            };

            let mid = start + offset;
            let left = nodes.len() as u32;
            nodes.push(Node {
                aabb: Aabb::EMPTY,
                offset: 0,
                count: 0,
            });
            nodes.push(Node {
                aabb: Aabb::EMPTY,
                offset: 0,
                count: 0,
            });
            nodes[node_index].offset = left;
            nodes[node_index].count = 0;

            stack.push((left as usize, start, mid));
            stack.push((left as usize + 1, mid, end));
        }

        Self {
            nodes,
            primitives,
            bounds: bounds.to_vec(),
        }
    }

    /// 各图元的包围盒，下标与构建时的输入一致。
    pub fn bounds(&self) -> &[Aabb] {
        &self.bounds
    }

    /// 图元数量。
    pub fn len(&self) -> usize {
        self.primitives.len()
    }

    /// 树是否为空。
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 节点数量，用于诊断构建质量。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 根节点包围盒，即所有图元之并。空树返回 [`Aabb::EMPTY`]。
    pub fn root_bounds(&self) -> Aabb {
        self.nodes.first().map_or(Aabb::EMPTY, |node| node.aabb)
    }

    /// 树高（根为 1）。空树返回 0。
    pub fn depth(&self) -> usize {
        if self.nodes.is_empty() {
            return 0;
        }
        let mut deepest = 0;
        let mut stack = vec![(0u32, 1usize)];
        while let Some((index, depth)) = stack.pop() {
            deepest = deepest.max(depth);
            let node = &self.nodes[index as usize];
            if !node.is_leaf() {
                stack.push((node.offset, depth + 1));
                stack.push((node.offset + 1, depth + 1));
            }
        }
        deepest
    }

    /// 只更新各节点的包围盒，不改变树的形状。
    ///
    /// 物体只是动了、增删没发生时，这比重建便宜一个数量级（O(n) vs O(n log n)），
    /// 代价是树的形状会随物体移动逐渐变差——移动幅度大时应当重建。
    ///
    /// `bounds` 的长度必须与构建时一致，否则本次调用被忽略。
    pub fn refit(&mut self, bounds: &[Aabb]) {
        if bounds.len() != self.primitives.len() {
            return;
        }
        self.bounds.copy_from_slice(bounds);

        // 子节点下标恒大于父节点，所以倒序遍历一遍即可自下而上更新完。
        for index in (0..self.nodes.len()).rev() {
            let node = self.nodes[index];
            let aabb = if node.is_leaf() {
                let range = node.offset as usize..node.offset as usize + node.count as usize;
                self.primitives[range]
                    .iter()
                    .fold(Aabb::EMPTY, |acc, &p| acc.union(&bounds[p as usize]))
            } else {
                let left = self.nodes[node.offset as usize].aabb;
                let right = self.nodes[node.offset as usize + 1].aabb;
                left.union(&right)
            };
            self.nodes[index].aabb = aabb;
        }
    }

    /// 遍历整棵树，把通过判定的图元下标追加到 `out`。
    ///
    /// `classify` 返回 [`Intersection::Inside`] 时整棵子树直接接受，不再往下判定——
    /// 这正是层次结构相对逐个判定的收益来源。
    pub fn query<F>(&self, classify: F, out: &mut Vec<u32>)
    where
        F: FnMut(&Aabb) -> Intersection,
    {
        if !self.nodes.is_empty() {
            self.query_subtree(0, classify, out);
        }
    }

    /// 只遍历以 `root` 为根的子树。配合 [`subtree_roots`](Bvh::subtree_roots) 可并行剔除。
    pub fn query_subtree<F>(&self, root: u32, mut classify: F, out: &mut Vec<u32>)
    where
        F: FnMut(&Aabb) -> Intersection,
    {
        if self.nodes.is_empty() {
            return;
        }

        let mut stack = vec![root];
        while let Some(index) = stack.pop() {
            let node = self.nodes[index as usize];
            match classify(&node.aabb) {
                Intersection::Outside => continue,
                Intersection::Inside => self.collect_subtree(index, out),
                Intersection::Intersects => {
                    if node.is_leaf() {
                        // 叶子只是「一批图元」，还得逐个判定，否则返回的是超集。
                        let range =
                            node.offset as usize..node.offset as usize + node.count as usize;
                        for &primitive in &self.primitives[range] {
                            if classify(&self.bounds[primitive as usize]) != Intersection::Outside {
                                out.push(primitive);
                            }
                        }
                    } else {
                        // 先压右后压左，弹出时左子树在前，结果顺序与构建顺序一致。
                        stack.push(node.offset + 1);
                        stack.push(node.offset);
                    }
                }
            }
        }
    }

    /// 把树切成若干互不重叠的子树，用于分片并行遍历。
    ///
    /// 返回的子树根覆盖全部图元且不重复。数量从根开始逐层翻倍，
    /// 直到不少于 `min_roots` 或已经全是叶子，所以实际数量可能略多于请求值。
    pub fn subtree_roots(&self, min_roots: usize) -> Vec<u32> {
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let mut frontier = vec![0u32];
        while frontier.len() < min_roots {
            let mut next = Vec::with_capacity(frontier.len() * 2);
            let mut expanded = false;
            for &index in &frontier {
                let node = self.nodes[index as usize];
                if node.is_leaf() {
                    next.push(index);
                } else {
                    next.push(node.offset);
                    next.push(node.offset + 1);
                    expanded = true;
                }
            }
            // 全是叶子了，再切也切不出更多分片。
            if !expanded {
                break;
            }
            frontier = next;
        }
        frontier
    }

    /// 无条件收下整棵子树的图元。
    fn collect_subtree(&self, root: u32, out: &mut Vec<u32>) {
        let mut stack = vec![root];
        while let Some(index) = stack.pop() {
            let node = self.nodes[index as usize];
            if node.is_leaf() {
                let range = node.offset as usize..node.offset as usize + node.count as usize;
                out.extend_from_slice(&self.primitives[range]);
            } else {
                stack.push(node.offset + 1);
                stack.push(node.offset);
            }
        }
    }
}

/// 为一段图元找分割点，返回左半部分的长度（相对 `slice` 起点）。
///
/// 返回 [`None`] 表示「不如就留作叶子」。函数会就地重排 `slice`。
fn find_split(slice: &mut [u32], centroids: &[Vec3], aabb: &Aabb, count: usize) -> Option<usize> {
    // 按质心分布而非包围盒分布来切：包围盒会互相重叠，质心不会。
    let mut centroid_bounds = Aabb::EMPTY;
    for &p in slice.iter() {
        centroid_bounds.expand(centroids[p as usize]);
    }

    let axis = centroid_bounds.largest_axis();
    let lo = axis_value(centroid_bounds.min, axis);
    let hi = axis_value(centroid_bounds.max, axis);
    // 所有质心重合，怎么切都没意义。
    // 写成否定式而不是 `<=`：这样 NaN 也会落进这一支，而不是带着 NaN 继续分箱。
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(hi - lo > 1e-6) {
        return None;
    }

    // ── 分箱：把每个图元丢进 BIN_COUNT 个桶之一 ──
    let scale = BIN_COUNT as f32 / (hi - lo);
    let bin_of = |p: u32| -> usize {
        let value = axis_value(centroids[p as usize], axis);
        (((value - lo) * scale) as usize).min(BIN_COUNT - 1)
    };

    let mut bin_bounds = [Aabb::EMPTY; BIN_COUNT];
    let mut bin_counts = [0usize; BIN_COUNT];
    for &p in slice.iter() {
        let bin = bin_of(p);
        bin_counts[bin] += 1;
        bin_bounds[bin] = bin_bounds[bin].union(&centroid_bounds_of(centroids, p));
    }

    // ── 前后缀扫描，得到每个候选分割点两侧的面积与数量 ──
    let mut left_area = [0.0f32; BIN_COUNT - 1];
    let mut left_count = [0usize; BIN_COUNT - 1];
    let mut accumulated = Aabb::EMPTY;
    let mut running = 0usize;
    for bin in 0..BIN_COUNT - 1 {
        accumulated = accumulated.union(&bin_bounds[bin]);
        running += bin_counts[bin];
        left_area[bin] = accumulated.surface_area();
        left_count[bin] = running;
    }

    let mut best_cost = f32::INFINITY;
    let mut best_bin = usize::MAX;
    let parent_area = aabb.surface_area();
    accumulated = Aabb::EMPTY;
    running = 0;
    for bin in (1..BIN_COUNT).rev() {
        accumulated = accumulated.union(&bin_bounds[bin]);
        running += bin_counts[bin];
        let (n_left, n_right) = (left_count[bin - 1], running);
        if n_left == 0 || n_right == 0 {
            continue;
        }
        // SAH：子节点被射线击中的概率正比于其表面积。
        let cost = TRAVERSAL_COST
            + (left_area[bin - 1] * n_left as f32 + accumulated.surface_area() * n_right as f32)
                / parent_area.max(f32::EPSILON);
        if cost < best_cost {
            best_cost = cost;
            best_bin = bin;
        }
    }

    // 切开还不如整体判定，且图元数不算多——那就留作叶子。
    if best_bin == usize::MAX || (best_cost >= count as f32 && count <= MAX_PRIMS_IN_NODE) {
        return None;
    }

    let mid = partition(slice, |p| bin_of(p) < best_bin);
    if mid == 0 || mid == count {
        // 分箱量化导致一边空了：退回中位数切分，保证树能继续往下长。
        let middle = count / 2;
        slice.select_nth_unstable_by(middle, |&a, &b| {
            axis_value(centroids[a as usize], axis)
                .total_cmp(&axis_value(centroids[b as usize], axis))
        });
        return Some(middle);
    }
    Some(mid)
}

/// 质心当作退化包围盒——分箱只关心质心的分布范围。
fn centroid_bounds_of(centroids: &[Vec3], primitive: u32) -> Aabb {
    let c = centroids[primitive as usize];
    Aabb::new(c, c)
}

fn axis_value(v: Vec3, axis: usize) -> f32 {
    match axis {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// 就地把满足 `predicate` 的元素挪到前面，返回前半部分长度。
fn partition(slice: &mut [u32], predicate: impl Fn(u32) -> bool) -> usize {
    let mut boundary = 0;
    for index in 0..slice.len() {
        if predicate(slice[index]) {
            slice.swap(boundary, index);
            boundary += 1;
        }
    }
    boundary
}

#[cfg(test)]
mod test {
    use super::*;

    /// 沿 x 轴等距排开的 `count` 个单位盒。
    fn line_of_boxes(count: usize) -> Vec<Aabb> {
        (0..count)
            .map(|i| {
                Aabb::from_center_half_extents(
                    Vec3::new(i as f32 * 2.0, 0.0, 0.0),
                    Vec3::splat(0.5),
                )
            })
            .collect()
    }

    /// 一个伪随机散布的场景，用来跟暴力遍历对拍。
    fn scattered_boxes(count: usize) -> Vec<Aabb> {
        // 固定种子的线性同余，保证每次跑到的是同一批数据。
        let mut state = 0x2545_F491u32;
        let mut next = |range: f32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1 << 24) as f32 * range - range * 0.5
        };
        (0..count)
            .map(|_| {
                let center = Vec3::new(next(200.0), next(60.0), next(200.0));
                Aabb::from_center_half_extents(center, Vec3::splat(0.5 + next(1.0).abs()))
            })
            .collect()
    }

    fn brute_force(bounds: &[Aabb], query: &Aabb) -> Vec<u32> {
        bounds
            .iter()
            .enumerate()
            .filter(|(_, aabb)| aabb.intersects(query))
            .map(|(index, _)| index as u32)
            .collect()
    }

    fn query_sorted(bvh: &Bvh, query: &Aabb) -> Vec<u32> {
        let mut hits = Vec::new();
        bvh.query(|aabb| aabb.classify_against(query), &mut hits);
        hits.sort_unstable();
        hits
    }

    #[test]
    fn empty_input_builds_empty_tree() {
        let bvh = Bvh::build(&[]);

        assert!(bvh.is_empty());
        assert_eq!(bvh.len(), 0);
        assert_eq!(bvh.depth(), 0);
        assert!(bvh.root_bounds().is_empty());
        // 查询空树不该 panic。
        assert!(query_sorted(&bvh, &Aabb::new(-Vec3::ONE, Vec3::ONE)).is_empty());
    }

    #[test]
    fn single_primitive_is_found() {
        let bounds = line_of_boxes(1);
        let bvh = Bvh::build(&bounds);

        assert_eq!(bvh.len(), 1);
        assert_eq!(bvh.node_count(), 1);
        assert_eq!(
            query_sorted(&bvh, &Aabb::new(-Vec3::ONE, Vec3::ONE)),
            vec![0]
        );
    }

    #[test]
    fn query_matches_brute_force() {
        let bounds = scattered_boxes(2000);
        let bvh = Bvh::build(&bounds);

        // 几个尺度不同的查询体：小盒、半场、包住一切。
        for half in [2.0f32, 30.0, 500.0] {
            let query =
                Aabb::from_center_half_extents(Vec3::new(10.0, 0.0, -20.0), Vec3::splat(half));
            assert_eq!(
                query_sorted(&bvh, &query),
                brute_force(&bounds, &query),
                "half = {half} 时 BVH 与暴力遍历的结果不一致"
            );
        }
    }

    #[test]
    fn every_primitive_appears_exactly_once() {
        let bounds = scattered_boxes(500);
        let bvh = Bvh::build(&bounds);

        // 查询体罩住整个场景，应当一个不漏、一个不重。
        let hits = query_sorted(&bvh, &Aabb::new(Vec3::splat(-1e4), Vec3::splat(1e4)));

        assert_eq!(hits, (0..500u32).collect::<Vec<_>>());
    }

    #[test]
    fn node_bounds_contain_children() {
        let bounds = scattered_boxes(300);
        let bvh = Bvh::build(&bounds);

        // 这是 BVH 的核心不变式：父节点包住子节点，否则剪枝会剪掉可见物体。
        for node in &bvh.nodes {
            if node.is_leaf() {
                for &p in &bvh.primitives
                    [node.offset as usize..node.offset as usize + node.count as usize]
                {
                    assert!(node.aabb.contains_aabb(&bounds[p as usize]));
                }
            } else {
                assert!(
                    node.aabb
                        .contains_aabb(&bvh.nodes[node.offset as usize].aabb)
                );
                assert!(
                    node.aabb
                        .contains_aabb(&bvh.nodes[node.offset as usize + 1].aabb)
                );
            }
        }
    }

    #[test]
    fn identical_centroids_do_not_hang() {
        // 一堆完全重合的盒子：质心分不开，SAH 无从下手，只能留作一个叶子。
        let bounds = vec![Aabb::new(-Vec3::ONE, Vec3::ONE); 64];
        let bvh = Bvh::build(&bounds);

        assert_eq!(bvh.len(), 64);
        assert_eq!(
            query_sorted(&bvh, &Aabb::new(-Vec3::ONE, Vec3::ONE)).len(),
            64
        );
    }

    #[test]
    fn refit_tracks_moved_primitives() {
        let mut bounds = line_of_boxes(64);
        let mut bvh = Bvh::build(&bounds);

        // 把 0 号盒挪到很远处，refit 后原地不该再查到它、新位置应当能查到。
        bounds[0] = Aabb::from_center_half_extents(Vec3::new(0.0, 500.0, 0.0), Vec3::splat(0.5));
        bvh.refit(&bounds);

        let here = Aabb::from_center_half_extents(Vec3::ZERO, Vec3::splat(0.4));
        let there = Aabb::from_center_half_extents(Vec3::new(0.0, 500.0, 0.0), Vec3::splat(0.4));

        assert!(query_sorted(&bvh, &here).is_empty());
        assert_eq!(query_sorted(&bvh, &there), vec![0]);
    }

    #[test]
    fn refit_ignores_mismatched_input() {
        let bounds = line_of_boxes(8);
        let mut bvh = Bvh::build(&bounds);
        let before = bvh.root_bounds();

        bvh.refit(&bounds[..4]);

        assert_eq!(bvh.root_bounds(), before);
    }

    #[test]
    fn subtree_roots_partition_all_primitives() {
        let bounds = scattered_boxes(400);
        let bvh = Bvh::build(&bounds);

        let roots = bvh.subtree_roots(8);
        assert!(roots.len() >= 8);

        let mut collected = Vec::new();
        for root in roots {
            bvh.collect_subtree(root, &mut collected);
        }
        collected.sort_unstable();

        // 分片必须既不重叠也不遗漏，否则并行剔除会漏画或重画。
        assert_eq!(collected, (0..400u32).collect::<Vec<_>>());
    }

    #[test]
    fn subtree_roots_stop_at_leaves() {
        let bounds = line_of_boxes(3);
        let bvh = Bvh::build(&bounds);

        // 只有一个叶子，再怎么要求分片也只能给一个。
        assert_eq!(bvh.subtree_roots(64).len(), 1);
    }

    #[test]
    fn tree_stays_shallow() {
        let bounds = scattered_boxes(4096);
        let bvh = Bvh::build(&bounds);

        // 理想树高是 log2(4096/4) = 10；放宽到 3 倍，退化成链表时能报警。
        assert!(bvh.depth() <= 30, "树高 {} 过深，构建可能退化", bvh.depth());
    }

    #[test]
    fn inside_shortcut_yields_same_result_as_full_test() {
        let bounds = scattered_boxes(300);
        let bvh = Bvh::build(&bounds);
        let query = Aabb::from_center_half_extents(Vec3::ZERO, Vec3::splat(40.0));

        // 关掉 Inside 快路径（只回答 Outside/Intersects），结果必须一致。
        let mut without_shortcut = Vec::new();
        bvh.query(
            |aabb| {
                if aabb.intersects(&query) {
                    Intersection::Intersects
                } else {
                    Intersection::Outside
                }
            },
            &mut without_shortcut,
        );
        without_shortcut.sort_unstable();

        assert_eq!(without_shortcut, query_sorted(&bvh, &query));
    }
}
