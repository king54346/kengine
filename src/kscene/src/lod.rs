//! 细节层级（LOD）：按到相机的距离，在几个子节点里只显示一个。
//!
//! 对应 three.js 的 `THREE.LOD`。挂在一个父节点上，第 `i` 级对应这个节点的
//! 第 `i` 个子节点：
//!
//! ```no_run
//! use kscene::{Lod, Node, Scene};
//! use kmesh::Mesh;
//!
//! let mut scene = Scene::new();
//! let lod = scene.add_node(
//!     Node::new("Rock").with_lod(Lod::new().with_level(0.0).with_level(50.0).with_level(200.0)),
//! );
//! for detail in [4, 2, 0] {
//!     let child = scene.add_node(Node::new("level").with_mesh(Mesh::icosphere(detail)));
//!     scene.link_nodes(child, lod);
//! }
//! ```
//!
//! # 选级只改「这一帧画不画」，不改 `visible`
//!
//! [`Scene::update`](crate::Scene::update) 在沿树算世界变换的那一趟里顺手
//! 选级：没选中的子树在这一帧的 `global_visible` 是假，于是不进剔除结构、
//! 不画、不投影。节点自己的 `visible` 字段**原样不动**——那是用户的开关，
//! 引擎要是改它，用户再也分不清「是我关的还是 LOD 关的」，隐藏整个 LOD
//! 物体之后再打开，也会被上一次选级的结果污染。
//!
//! # 距离从哪台相机量
//!
//! 活动相机（[`Scene::active_camera`](crate::Scene::active_camera) 的同一台）。
//! 它的世界位置是在 `update` 一开始沿父链现算的，所以相机在这一帧里被挪动过，
//! 选级用的也是新位置，不会慢一帧。场景里没有相机时不选级，保持上一次的结果。
//!
//! # 滞回（hysteresis）
//!
//! 相机刚好停在分界距离附近时，浮点抖动会让两级来回切，看起来就是在闪。
//! 给某一级设 `hysteresis = 0.1` 之后，**已经显示着**这一级时，要退回更近的
//! 那一级得再靠近 10%——进和出的门槛错开，就不会来回跳。

use kcore::visitor::{Visit, VisitResult, Visitor};

/// 一级细节。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodLevel {
    /// 从这个距离起显示这一级（世界单位）。
    pub distance: f32,
    /// 滞回比例，0 表示不设。见模块文档。
    pub hysteresis: f32,
}

/// LOD 组件。见模块文档。
#[derive(Debug, Clone, PartialEq)]
pub struct Lod {
    levels: Vec<LodLevel>,
    current: usize,
    /// 是否每帧按相机距离自动选级。关掉之后由 [`set_current`](Self::set_current)
    /// 手动指定——做「按屏幕占比选级」或者调试时强制看某一级用得到。
    pub auto_update: bool,
}

impl Default for Lod {
    fn default() -> Self {
        Self::new()
    }
}

impl Lod {
    /// 没有任何级别的 LOD（此时所有子节点照常显示）。
    pub fn new() -> Self {
        Self {
            levels: Vec::new(),
            current: 0,
            auto_update: true,
        }
    }

    /// 追加一级，从 `distance` 起显示。
    pub fn with_level(mut self, distance: f32) -> Self {
        self.add_level(distance, 0.0);
        self
    }

    /// 追加一级并设滞回。
    pub fn with_level_hysteresis(mut self, distance: f32, hysteresis: f32) -> Self {
        self.add_level(distance, hysteresis);
        self
    }

    /// 追加一级。
    ///
    /// 级别总是按距离从近到远排好的——和 three.js 一样，乱序加入时插到
    /// 对应位置。**级别下标就是子节点下标**，所以乱序加的时候，
    /// 子节点也得按排好之后的顺序挂。返回这一级最终的下标。
    pub fn add_level(&mut self, distance: f32, hysteresis: f32) -> usize {
        let distance = if distance.is_finite() { distance.abs() } else { f32::MAX };
        let level = LodLevel {
            distance,
            // `NaN.clamp()` 还是 NaN，得单独挡。
            hysteresis: if hysteresis.is_nan() { 0.0 } else { hysteresis.clamp(0.0, 1.0) },
        };
        let index = self.levels.partition_point(|l| l.distance <= distance);
        self.levels.insert(index, level);
        index
    }

    /// 全部级别，按距离从近到远。
    pub fn levels(&self) -> &[LodLevel] {
        &self.levels
    }

    /// 当前显示的级别。
    pub fn current(&self) -> usize {
        self.current
    }

    /// 手动指定当前级别（越界时夹到最后一级）。
    ///
    /// `auto_update` 开着的话下一帧会被距离选级覆盖掉。
    pub fn set_current(&mut self, level: usize) {
        self.current = level.min(self.levels.len().saturating_sub(1));
    }

    /// 给定距离该显示哪一级。考虑滞回，所以结果取决于当前级别。
    pub fn select(&self, distance: f32) -> usize {
        if self.levels.len() < 2 || distance.is_nan() {
            return 0;
        }
        for index in 1..self.levels.len() {
            let level = self.levels[index];
            let mut threshold = level.distance;
            // 已经在这一级（或更远）时，退回来要多走一段。
            if self.current >= index {
                threshold -= threshold * level.hysteresis;
            }
            if distance < threshold {
                return index - 1;
            }
        }
        self.levels.len() - 1
    }

    /// 按距离选级并记下结果，返回选中的级别。
    pub fn update(&mut self, distance: f32) -> usize {
        self.current = self.select(distance);
        self.current
    }

    /// 第 `child` 个子节点这一帧该不该画。不对应任何级别的子节点总是画。
    pub fn shows_child(&self, child: usize) -> bool {
        child >= self.levels.len() || child == self.current
    }
}

impl Visit for Lod {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        let mut distances: Vec<f32> = self.levels.iter().map(|l| l.distance).collect();
        let mut hysteresis: Vec<f32> = self.levels.iter().map(|l| l.hysteresis).collect();
        distances.visit("Distances", &mut region)?;
        hysteresis.visit("Hysteresis", &mut region)?;
        self.auto_update.visit("AutoUpdate", &mut region)?;
        if region.is_reading() {
            // 走 `add_level` 重新排一遍：存档被手改乱序也不会选错级。
            self.levels.clear();
            for (index, &distance) in distances.iter().enumerate() {
                self.add_level(distance, hysteresis.get(index).copied().unwrap_or(0.0));
            }
            // 当前级别是运行期状态，读回后由下一帧的选级决定。
            self.current = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_levels() -> Lod {
        Lod::new().with_level(0.0).with_level(50.0).with_level(300.0)
    }

    #[test]
    fn picks_the_farthest_level_whose_distance_is_reached() {
        let lod = three_levels();
        assert_eq!(lod.select(0.0), 0);
        assert_eq!(lod.select(49.9), 0);
        assert_eq!(lod.select(50.0), 1);
        assert_eq!(lod.select(299.0), 1);
        assert_eq!(lod.select(1e6), 2);
    }

    #[test]
    fn closer_than_the_first_level_still_shows_the_first_level() {
        // three.js 的行为：第 0 级的距离只是「从哪起」，更近的时候也显示它。
        let lod = Lod::new().with_level(50.0).with_level(300.0);
        assert_eq!(lod.select(1.0), 0);
    }

    #[test]
    fn levels_are_kept_sorted_by_distance() {
        let mut lod = Lod::new();
        assert_eq!(lod.add_level(300.0, 0.0), 0);
        assert_eq!(lod.add_level(0.0, 0.0), 0);
        assert_eq!(lod.add_level(50.0, 0.0), 1);
        let distances: Vec<f32> = lod.levels().iter().map(|l| l.distance).collect();
        assert_eq!(distances, vec![0.0, 50.0, 300.0]);
    }

    #[test]
    fn hysteresis_keeps_the_current_level_near_the_boundary() {
        let mut lod = Lod::new().with_level(0.0).with_level_hysteresis(100.0, 0.1);
        assert_eq!(lod.update(101.0), 1);
        // 退回到 95：没设滞回的话就回到第 0 级了，设了 10% 要到 90 以内才回。
        assert_eq!(lod.update(95.0), 1);
        assert_eq!(lod.update(89.0), 0);
        // 从近处往外走时门槛仍然是 100。
        assert_eq!(lod.update(99.0), 0);
        assert_eq!(lod.update(100.0), 1);
    }

    #[test]
    fn children_beyond_the_levels_are_always_shown() {
        let mut lod = three_levels();
        lod.update(60.0);
        assert!(!lod.shows_child(0));
        assert!(lod.shows_child(1));
        assert!(!lod.shows_child(2));
        assert!(lod.shows_child(3), "不对应级别的子节点（比如挂在上面的标签）照常显示");
    }

    #[test]
    fn nan_and_infinite_inputs_do_not_panic() {
        let mut lod = Lod::new();
        lod.add_level(f32::NAN, f32::INFINITY);
        lod.add_level(10.0, -1.0);
        lod.add_level(20.0, f32::NAN);
        assert_eq!(lod.select(f32::NAN), 0);
        assert!(lod.levels().iter().all(|l| (0.0..=1.0).contains(&l.hysteresis)));
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    use crate::{Camera, Mesh, Node, Scene};
    use kcore::pool::Handle;
    use kmath::Vec3;

    /// 一个三级 LOD 挂在原点，相机在 z = `distance` 处。
    fn scene_at(distance: f32) -> (Scene, Handle<Node>, Vec<Handle<Node>>, Handle<Node>) {
        let mut scene = Scene::new();
        let camera = scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_position(Vec3::new(0.0, 0.0, distance)),
        );
        let lod = scene.add_node(
            Node::new("Lod").with_lod(Lod::new().with_level(0.0).with_level(50.0).with_level(300.0)),
        );
        let levels = (0..3)
            .map(|detail| {
                let child = scene.add_node(Node::new("level").with_mesh(Mesh::icosphere(detail)));
                scene.link_nodes(child, lod);
                child
            })
            .collect();
        // 第一帧建相机索引，第二帧才量得到距离。
        scene.update();
        scene.update();
        (scene, lod, levels, camera)
    }

    fn shown(scene: &Scene, levels: &[Handle<Node>]) -> Vec<bool> {
        levels.iter().map(|&h| scene[h].global_visible).collect()
    }

    #[test]
    fn only_the_selected_level_is_drawn() {
        let (scene, lod, levels, _) = scene_at(100.0);
        assert_eq!(scene[lod].lod().unwrap().current(), 1);
        assert_eq!(shown(&scene, &levels), vec![false, true, false]);
        assert_eq!(scene.visible_meshes().count(), 1, "没选中的级别不该进绘制列表");
    }

    #[test]
    fn moving_the_camera_switches_level_in_the_same_frame() {
        let (mut scene, _, levels, camera) = scene_at(10.0);
        assert_eq!(shown(&scene, &levels), vec![true, false, false]);
        scene[camera].transform.position = Vec3::new(0.0, 0.0, 1000.0);
        scene.update();
        assert_eq!(shown(&scene, &levels), vec![false, false, true]);
    }

    #[test]
    fn user_visibility_flags_are_left_alone() {
        let (mut scene, lod, levels, _) = scene_at(100.0);
        for &h in &levels {
            assert!(scene[h].visible, "LOD 不能改写用户的 visible");
        }
        // 整个 LOD 物体被用户藏起来时，选中的那一级也不画。
        scene[lod].visible = false;
        scene.update();
        assert_eq!(shown(&scene, &levels), vec![false, false, false]);
    }

    #[test]
    fn manual_selection_sticks_when_auto_update_is_off() {
        let (mut scene, lod, levels, _) = scene_at(100.0);
        let component = scene[lod].lod_mut().unwrap();
        component.auto_update = false;
        component.set_current(2);
        scene.update();
        assert_eq!(shown(&scene, &levels), vec![false, false, true]);
    }

    #[test]
    fn lod_survives_a_scene_roundtrip() {
        let (mut scene, lod, _, _) = scene_at(100.0);
        let bytes = scene.save_to_vec().expect("存得下来");
        let restored = Scene::load_from_slice(&bytes, None).expect("读得回来");
        let levels: Vec<f32> =
            restored[lod].lod().expect("LOD 该原样回来").levels().iter().map(|l| l.distance).collect();
        assert_eq!(levels, vec![0.0, 50.0, 300.0]);
    }
}
