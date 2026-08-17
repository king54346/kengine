//! 剔除加速结构：一棵覆盖场景中所有可绘制节点的 BVH。
//!
//! 剔除原本是逐节点线性遍历，对象上万时每帧都要把整份列表走一遍。
//! 这里用 [`kmath::Bvh`] 把可绘制节点组织成层次结构：视锥外的整棵子树一次跳过，
//! 视锥内的整棵子树一次接受，只有跨在视锥边界上的分支才需要逐个判定。
//!
//! 结构由 [`Scene::update`](crate::Scene::update) 每帧维护，调用方不直接接触。

use crate::Node;
use kcamera::Frustum;
use kcore::pool::Handle;
use kmath::{Aabb, Bvh};
use ktask::{ComputeTaskPool, ParallelSlice, TaskPool};

/// 低于这个对象数就单线程剔除——分片、唤醒线程、合并结果本身也要开销，
/// 小场景里这些固定成本比剔除本身还贵。
const PARALLEL_THRESHOLD: usize = 4096;

/// 低于这个对象数，变换一变就整棵重建：这个规模下构建本身只有几十微秒，
/// 换来的是永远最优的树形。
const REBUILD_THRESHOLD: usize = 2048;

/// 大场景连续 refit 的上限。refit 只更新包围盒、不改树形，
/// 物体持续移动会让树形逐渐变差，隔一段时间必须重建一次。
const MAX_REFITS: u32 = 240;

/// 场景的剔除加速结构。
#[derive(Default)]
pub(crate) struct SceneCulling {
    /// 参与剔除的节点，下标即 BVH 里的图元号。
    handles: Vec<Handle<Node>>,
    /// 与 `handles` 一一对应的世界包围盒。
    bounds: Vec<Aabb>,
    bvh: Bvh,
    /// 本帧收集到的数据，`commit` 时与上一帧比对后再换上。
    next_handles: Vec<Handle<Node>>,
    next_bounds: Vec<Aabb>,
    refits: u32,
}

impl SceneCulling {
    /// 开始新一帧的收集。
    pub(crate) fn begin(&mut self) {
        self.next_handles.clear();
        self.next_bounds.clear();
    }

    /// 记下一个可绘制节点。
    pub(crate) fn push(&mut self, handle: Handle<Node>, aabb: Aabb) {
        self.next_handles.push(handle);
        self.next_bounds.push(aabb);
    }

    /// 收集完毕，按变化程度决定重建还是只更新包围盒。
    pub(crate) fn commit(&mut self) {
        if self.next_handles != self.handles {
            // 节点增删或可见性变化 → 图元编号全变了，只能重建。
            std::mem::swap(&mut self.handles, &mut self.next_handles);
            std::mem::swap(&mut self.bounds, &mut self.next_bounds);
            self.rebuild();
            return;
        }

        if self.next_bounds == self.bounds {
            // 什么都没动，上一帧的树继续用。
            return;
        }

        std::mem::swap(&mut self.bounds, &mut self.next_bounds);
        if self.bounds.len() <= REBUILD_THRESHOLD || self.refits >= MAX_REFITS {
            self.rebuild();
        } else {
            self.bvh.refit(&self.bounds);
            self.refits += 1;
        }
    }

    fn rebuild(&mut self) {
        self.bvh = Bvh::build(&self.bounds);
        self.refits = 0;
    }

    /// 参与剔除的节点数。
    pub(crate) fn len(&self) -> usize {
        self.handles.len()
    }

    /// 所有可绘制节点的包围盒之并，取自 BVH 根节点，O(1)。
    pub(crate) fn bounds(&self) -> Aabb {
        self.bvh.root_bounds()
    }

    /// 下标转节点句柄。
    pub(crate) fn handle(&self, index: u32) -> Handle<Node> {
        self.handles[index as usize]
    }

    /// 视锥剔除，把可见节点的下标追加到 `out`。
    ///
    /// 结果顺序是 BVH 的深度优先顺序，与线程数无关——并行路径按分片顺序合并，
    /// 而「整棵树的深度优先」恰好等于「有序切分后各子树深度优先的拼接」。
    pub(crate) fn cull(&self, frustum: &Frustum, out: &mut Vec<u32>) {
        if self.handles.len() < PARALLEL_THRESHOLD {
            self.bvh.query(|aabb| frustum.classify(aabb), out);
            return;
        }

        let pool = ComputeTaskPool::get_or_init(TaskPool::new);
        // 每个线程分几片，好让先做完的线程能接着领下一片，避免尾部空转。
        let roots = self.bvh.subtree_roots(pool.thread_num() * 4);
        let chunks = roots.par_splat_map(pool, None, |_, roots| {
            let mut local = Vec::new();
            for &root in roots {
                self.bvh
                    .query_subtree(root, |aabb| frustum.classify(aabb), &mut local);
            }
            local
        });

        for chunk in chunks {
            out.extend_from_slice(&chunk);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kcamera::Camera;
    use kmath::{Mat4, Vec3};

    /// 造一个规模可控的测试用剔除结构：物体排成网格。
    fn grid(side: usize, spacing: f32) -> SceneCulling {
        let mut culling = SceneCulling::default();
        culling.begin();
        for x in 0..side {
            for z in 0..side {
                let center = Vec3::new(
                    (x as f32 - side as f32 * 0.5) * spacing,
                    0.0,
                    (z as f32 - side as f32 * 0.5) * spacing,
                );
                culling.push(
                    Handle::new((x * side + z) as u32, 1),
                    Aabb::from_center_half_extents(center, Vec3::splat(0.4)),
                );
            }
        }
        culling.commit();
        culling
    }

    fn test_frustum() -> Frustum {
        let camera = Camera::default();
        let view = Mat4::look_at_rh(Vec3::new(0.0, 8.0, 30.0), Vec3::ZERO, Vec3::Y);
        Frustum::from_view_projection(camera.projection_matrix(16.0 / 9.0) * view)
    }

    fn brute_force(culling: &SceneCulling, frustum: &Frustum) -> Vec<u32> {
        culling
            .bounds
            .iter()
            .enumerate()
            .filter(|(_, aabb)| frustum.intersects(aabb))
            .map(|(index, _)| index as u32)
            .collect()
    }

    #[test]
    fn bvh_culling_matches_linear_culling() {
        let culling = grid(40, 2.0);
        let frustum = test_frustum();

        let mut visible = Vec::new();
        culling.cull(&frustum, &mut visible);
        visible.sort_unstable();

        // 加速结构不能改变可见集，否则画面就和逐个判定不一样了。
        assert_eq!(visible, brute_force(&culling, &frustum));
        assert!(!visible.is_empty() && visible.len() < culling.len());
    }

    #[test]
    fn parallel_culling_matches_linear_culling() {
        // 超过并行阈值，走分片路径。
        let culling = grid(80, 1.0);
        assert!(culling.len() >= PARALLEL_THRESHOLD);
        let frustum = test_frustum();

        let mut visible = Vec::new();
        culling.cull(&frustum, &mut visible);
        let ordered = visible.clone();
        visible.sort_unstable();

        assert_eq!(visible, brute_force(&culling, &frustum));
        // 分片合并后的顺序必须是确定的，否则绘制顺序会随线程调度抖动。
        assert_eq!(ordered, {
            let mut again = Vec::new();
            culling.cull(&frustum, &mut again);
            again
        });
    }

    #[test]
    fn unchanged_scene_reuses_the_tree() {
        let mut culling = grid(4, 2.0);
        let before = culling.bounds();

        // 收集到和上一帧一模一样的数据：不该有任何重建。
        let handles = culling.handles.clone();
        let bounds = culling.bounds.clone();
        culling.begin();
        for (handle, aabb) in handles.iter().zip(&bounds) {
            culling.push(*handle, *aabb);
        }
        culling.commit();

        assert_eq!(culling.bounds(), before);
        assert_eq!(culling.refits, 0);
    }

    #[test]
    fn moving_a_node_updates_the_bounds() {
        let mut culling = grid(4, 2.0);

        let handles = culling.handles.clone();
        let mut bounds = culling.bounds.clone();
        bounds[0] = Aabb::from_center_half_extents(Vec3::new(0.0, 900.0, 0.0), Vec3::splat(0.4));
        culling.begin();
        for (handle, aabb) in handles.iter().zip(&bounds) {
            culling.push(*handle, *aabb);
        }
        culling.commit();

        assert!(culling.bounds().max.y > 800.0);
    }

    #[test]
    fn removing_a_node_rebuilds() {
        let mut culling = grid(4, 2.0);
        let handles = culling.handles.clone();
        let bounds = culling.bounds.clone();

        culling.begin();
        for (handle, aabb) in handles.iter().zip(&bounds).skip(1) {
            culling.push(*handle, *aabb);
        }
        culling.commit();

        assert_eq!(culling.len(), handles.len() - 1);
        // 下标要跟着重排，否则查出来的会是别人的节点。
        assert_eq!(culling.handle(0), handles[1]);
    }

    #[test]
    fn empty_scene_is_handled() {
        let mut culling = SceneCulling::default();
        culling.begin();
        culling.commit();

        let mut visible = Vec::new();
        culling.cull(&test_frustum(), &mut visible);

        assert_eq!(culling.len(), 0);
        assert!(visible.is_empty());
        assert!(culling.bounds().is_empty());
    }
}
