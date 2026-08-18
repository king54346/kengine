//! 流式加载：大地图按区块随观察者进出而装卸。
//!
//! # 形态：显式分区 + 距离激活
//!
//! 一个**区块**就是一个存盘的场景文件加一个位置和半径。观察者靠近就把它并进
//! 主场景，走远就把那棵子树整个删掉。为什么不是「引擎按网格自动切分」：
//! 自动切分需要知道什么东西该跟什么东西一起加载（一栋楼和它的门、一个房间和
//! 它的灯），而这个信息只有做关卡的人有。显式分区把这个决定权留在了它该在的地方，
//! 同时「按距离自动装卸」的便利一点没少。
//!
//! 手动装卸也走同一套：[`Streaming::force_load`] / [`Streaming::force_unload`]，
//! 想完全自己控制就把两个距离设成无穷大。
//!
//! # 迟滞
//!
//! 加载距离**必须**小于卸载距离。相等的话，观察者站在边界上会每帧装一次卸一次，
//! 帧率直接归零——这是流式加载最经典的翻车方式，所以
//! [`Streaming::with_distances`] 会强制拉开两者。
//!
//! # 预算
//!
//! 一次 [`update`](Streaming::update) 最多装载 [`max_loads_per_update`] 个区块。
//! 玩家瞬移到地图另一头时，没有预算就要在一帧里读进几十个区块，
//! 表现为长达数秒的卡死。宁可让远处的区块晚几帧出现。

use crate::{Node, Scene};
use kasset::ResourceManager;
use kcore::pool::Handle;
use kmath::Vec3;
use std::path::PathBuf;

/// 一个区块的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// 不在场景里。
    Unloaded,
    /// 已并进主场景。
    Loaded,
    /// 上次加载失败了，不再重试。
    ///
    /// 不断重试一个读不出来的文件只会每帧刷一屏错误日志，
    /// 而问题（文件缺失、格式不对）重试一万次也不会自己好。
    Failed,
}

/// 一个流式区块。
#[derive(Debug, Clone)]
pub struct Cell {
    /// 区块名，用于查找与日志。
    pub name: String,
    /// 场景文件路径。
    pub path: PathBuf,
    /// 区块中心，世界空间。
    pub center: Vec3,
    /// 区块半径。距离判定按「观察者到球面的距离」算，
    /// 所以一个很大的区块不会因为中心点远就被卸掉。
    pub radius: f32,
    state: CellState,
    /// 并进来之后那棵子树的根。
    root: Handle<Node>,
}

impl Cell {
    /// 描述一个区块。
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, center: Vec3, radius: f32) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            center,
            radius: radius.max(0.0),
            state: CellState::Unloaded,
            root: Handle::NONE,
        }
    }

    /// 当前状态。
    pub fn state(&self) -> CellState {
        self.state
    }

    /// 是否已经在场景里。
    pub fn is_loaded(&self) -> bool {
        self.state == CellState::Loaded
    }

    /// 并进主场景后那棵子树的根；没加载时是 [`Handle::NONE`]。
    pub fn root(&self) -> Handle<Node> {
        self.root
    }

    /// 观察者到本区块的距离。落在区块内时为 0。
    pub fn distance_to(&self, viewer: Vec3) -> f32 {
        (viewer - self.center).length() - self.radius
    }
}

/// 一次 [`Streaming::update`] 做了什么。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamingReport {
    /// 这一轮装载的区块名。
    pub loaded: Vec<String>,
    /// 这一轮卸载的区块名。
    pub unloaded: Vec<String>,
    /// 因为超出预算而推迟到下一轮的区块数。
    pub deferred: usize,
}

impl StreamingReport {
    /// 这一轮有没有动过场景。
    pub fn changed(&self) -> bool {
        !self.loaded.is_empty() || !self.unloaded.is_empty()
    }
}

/// 流式区块的调度器。
#[derive(Debug)]
pub struct Streaming {
    cells: Vec<Cell>,
    load_distance: f32,
    unload_distance: f32,
    /// 一次 `update` 最多装载几个区块。
    pub max_loads_per_update: usize,
    /// 区块子树统一挂在这个节点下，方便整体隐藏或统计。
    container: Handle<Node>,
}

impl Streaming {
    /// 建一个空的调度器，装卸距离取默认值（100 / 140）。
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            load_distance: 100.0,
            unload_distance: 140.0,
            max_loads_per_update: 1,
            container: Handle::NONE,
        }
    }

    /// 设定装卸距离。
    ///
    /// 卸载距离会被强制拉到至少比加载距离大 10%——两者相等时观察者站在
    /// 边界上会每帧装一次卸一次。
    pub fn with_distances(mut self, load: f32, unload: f32) -> Self {
        self.load_distance = load.max(0.0);
        self.unload_distance = unload.max(self.load_distance * 1.1 + f32::EPSILON);
        self
    }

    /// 设定单次装载预算。
    pub fn with_load_budget(mut self, budget: usize) -> Self {
        self.max_loads_per_update = budget.max(1);
        self
    }

    /// 加载距离。
    pub fn load_distance(&self) -> f32 {
        self.load_distance
    }

    /// 卸载距离。
    pub fn unload_distance(&self) -> f32 {
        self.unload_distance
    }

    /// 登记一个区块。
    pub fn add_cell(&mut self, cell: Cell) {
        self.cells.push(cell);
    }

    /// 全部区块。
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// 按名字找区块。
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|cell| cell.name == name)
    }

    /// 已装载的区块数。
    pub fn loaded_count(&self) -> usize {
        self.cells.iter().filter(|cell| cell.is_loaded()).count()
    }

    /// 区块子树的容器节点。第一次装载时按需创建。
    pub fn container(&self) -> Handle<Node> {
        self.container
    }

    /// 按观察者位置装卸区块。
    ///
    /// 每帧调用。`manager` 用来解析区块场景里的资源引用。
    pub fn update(
        &mut self,
        scene: &mut Scene,
        viewer: Vec3,
        manager: Option<&ResourceManager>,
    ) -> StreamingReport {
        let mut report = StreamingReport::default();

        // 先卸载再装载：腾出来的内存立刻能给这一轮要装的区块用。
        for index in 0..self.cells.len() {
            if self.cells[index].state != CellState::Loaded {
                continue;
            }
            if self.cells[index].distance_to(viewer) <= self.unload_distance {
                continue;
            }
            let name = self.cells[index].name.clone();
            let root = self.cells[index].root;
            scene.remove_node(root);
            self.cells[index].state = CellState::Unloaded;
            self.cells[index].root = Handle::NONE;
            report.unloaded.push(name);
        }

        // 装载：按距离从近到远，先装最要紧的。
        let mut candidates: Vec<usize> = (0..self.cells.len())
            .filter(|index| {
                self.cells[*index].state == CellState::Unloaded
                    && self.cells[*index].distance_to(viewer) <= self.load_distance
            })
            .collect();
        candidates.sort_by(|a, b| {
            self.cells[*a]
                .distance_to(viewer)
                .total_cmp(&self.cells[*b].distance_to(viewer))
        });

        for index in candidates {
            if report.loaded.len() >= self.max_loads_per_update {
                report.deferred += 1;
                continue;
            }
            if self.load_cell(scene, index, manager) {
                report.loaded.push(self.cells[index].name.clone());
            }
        }

        report
    }

    /// 不看距离，强制装载一个区块。已装载时什么都不做，返回 `false`。
    pub fn force_load(
        &mut self,
        scene: &mut Scene,
        name: &str,
        manager: Option<&ResourceManager>,
    ) -> bool {
        let Some(index) = self.cells.iter().position(|cell| cell.name == name) else {
            return false;
        };
        if self.cells[index].state == CellState::Loaded {
            return false;
        }
        // 手动装载时把失败状态清掉：调用方显然是想再试一次。
        self.cells[index].state = CellState::Unloaded;
        self.load_cell(scene, index, manager)
    }

    /// 不看距离，强制卸载一个区块。
    pub fn force_unload(&mut self, scene: &mut Scene, name: &str) -> bool {
        let Some(index) = self.cells.iter().position(|cell| cell.name == name) else {
            return false;
        };
        if self.cells[index].state != CellState::Loaded {
            return false;
        }
        scene.remove_node(self.cells[index].root);
        self.cells[index].state = CellState::Unloaded;
        self.cells[index].root = Handle::NONE;
        true
    }

    /// 卸载全部区块。
    pub fn unload_all(&mut self, scene: &mut Scene) -> usize {
        let names: Vec<String> = self
            .cells
            .iter()
            .filter(|cell| cell.is_loaded())
            .map(|cell| cell.name.clone())
            .collect();
        let count = names.len();
        for name in names {
            self.force_unload(scene, &name);
        }
        count
    }

    fn load_cell(
        &mut self,
        scene: &mut Scene,
        index: usize,
        manager: Option<&ResourceManager>,
    ) -> bool {
        let path = self.cells[index].path.clone();

        let loaded = match Scene::load(&path, manager) {
            Ok(loaded) => loaded,
            Err(error) => {
                klog::error!(
                    "流式区块「{}」加载失败（{}）：{error:?}",
                    self.cells[index].name,
                    path.display()
                );
                // 标记为失败并**不再重试**：文件缺失或格式不对，
                // 重试一万次也不会自己好，只会每帧刷一屏日志。
                self.cells[index].state = CellState::Failed;
                return false;
            }
        };

        let container = self.ensure_container(scene);
        let root = scene.merge(loaded, container);
        if root.is_none() {
            self.cells[index].state = CellState::Failed;
            return false;
        }

        // 区块场景的根节点在这里成了一棵子树的根，给它换个认得出的名字。
        if let Some(node) = scene.try_get_mut(root) {
            node.name = format!("Cell:{}", self.cells[index].name);
        }

        self.cells[index].state = CellState::Loaded;
        self.cells[index].root = root;
        true
    }

    fn ensure_container(&mut self, scene: &mut Scene) -> Handle<Node> {
        if scene.try_get(self.container).is_some() {
            return self.container;
        }
        self.container = scene.add_node(Node::new("Streaming"));
        self.container
    }
}

impl Default for Streaming {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Collider, RigidBody, Skin};
    use kmath::Mat4;
    use kmesh::Mesh;

    /// 造一个区块场景文件，返回路径。
    fn write_cell(directory: &std::path::Path, name: &str, build: impl FnOnce(&mut Scene)) -> PathBuf {
        let mut scene = Scene::new();
        build(&mut scene);
        let path = directory.join(format!("{name}.scene"));
        scene.save(&path).unwrap();
        path
    }

    fn stage(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("kengine_stream_{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn a_cell_loads_when_the_viewer_approaches_and_unloads_when_they_leave() {
        let directory = stage("basic");
        let path = write_cell(&directory, "a", |scene| {
            scene.add_node(Node::new("rock").with_mesh(Mesh::cube()));
        });

        let mut scene = Scene::new();
        let mut streaming = Streaming::new().with_distances(10.0, 20.0);
        streaming.add_cell(Cell::new("a", path, Vec3::ZERO, 1.0));

        // 远处：什么都不装。
        let report = streaming.update(&mut scene, Vec3::X * 100.0, None);
        assert!(!report.changed());
        assert_eq!(streaming.loaded_count(), 0);

        // 走近：装上。
        let report = streaming.update(&mut scene, Vec3::X * 5.0, None);
        assert_eq!(report.loaded, vec!["a"]);
        scene.update();
        assert_eq!(scene.drawable_count(), 1, "区块里的网格没进场景");

        // 走远：卸掉。
        let report = streaming.update(&mut scene, Vec3::X * 100.0, None);
        assert_eq!(report.unloaded, vec!["a"]);
        scene.update();
        assert_eq!(scene.drawable_count(), 0, "卸载后节点还在");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn standing_on_the_boundary_does_not_thrash() {
        // 装卸距离相等的话，观察者站在边界上会每帧装一次卸一次，帧率归零。
        let directory = stage("hysteresis");
        let path = write_cell(&directory, "a", |scene| {
            scene.add_node(Node::new("rock").with_mesh(Mesh::cube()));
        });

        let mut scene = Scene::new();
        let mut streaming = Streaming::new().with_distances(10.0, 10.0);
        assert!(
            streaming.unload_distance() > streaming.load_distance(),
            "两个距离被允许相等了"
        );

        streaming.add_cell(Cell::new("a", path, Vec3::ZERO, 0.0));
        streaming.update(&mut scene, Vec3::X * 10.0, None);
        assert_eq!(streaming.loaded_count(), 1);

        // 就站在加载边界上反复 update，不该出现任何装卸。
        for _ in 0..20 {
            let report = streaming.update(&mut scene, Vec3::X * 10.0, None);
            assert!(!report.changed(), "在边界上反复装卸了");
        }

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn distance_is_measured_to_the_cell_surface_not_its_centre() {
        // 一个很大的区块不该因为中心点远就被判定成「远」。
        let cell = Cell::new("big", "unused", Vec3::ZERO, 50.0);

        assert_eq!(cell.distance_to(Vec3::ZERO), -50.0);
        assert_eq!(cell.distance_to(Vec3::X * 50.0), 0.0);
        assert_eq!(cell.distance_to(Vec3::X * 60.0), 10.0);
    }

    #[test]
    fn the_load_budget_spreads_work_across_frames() {
        // 玩家瞬移到地图另一头时，没有预算就要在一帧里读进几十个区块。
        let directory = stage("budget");
        let mut streaming = Streaming::new()
            .with_distances(1000.0, 2000.0)
            .with_load_budget(2);
        for index in 0..5 {
            let path = write_cell(&directory, &format!("c{index}"), |scene| {
                scene.add_node(Node::new("x").with_mesh(Mesh::cube()));
            });
            streaming.add_cell(Cell::new(format!("c{index}"), path, Vec3::ZERO, 1.0));
        }

        let mut scene = Scene::new();
        let report = streaming.update(&mut scene, Vec3::ZERO, None);

        assert_eq!(report.loaded.len(), 2);
        assert_eq!(report.deferred, 3, "超预算的区块该记成推迟");

        // 后续几帧把剩下的补齐。
        streaming.update(&mut scene, Vec3::ZERO, None);
        streaming.update(&mut scene, Vec3::ZERO, None);
        assert_eq!(streaming.loaded_count(), 5);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_nearest_cell_loads_first() {
        let directory = stage("order");
        let mut streaming = Streaming::new()
            .with_distances(1000.0, 2000.0)
            .with_load_budget(1);
        streaming.add_cell(Cell::new(
            "far",
            write_cell(&directory, "far", |s| {
                s.add_node(Node::new("x"));
            }),
            Vec3::X * 500.0,
            1.0,
        ));
        streaming.add_cell(Cell::new(
            "near",
            write_cell(&directory, "near", |s| {
                s.add_node(Node::new("x"));
            }),
            Vec3::X * 5.0,
            1.0,
        ));

        let mut scene = Scene::new();
        let report = streaming.update(&mut scene, Vec3::ZERO, None);

        assert_eq!(report.loaded, vec!["near"], "先装的不是最近的那个");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_broken_cell_fails_once_and_is_not_retried() {
        // 不断重试一个读不出来的文件只会每帧刷一屏错误日志。
        let mut scene = Scene::new();
        let mut streaming = Streaming::new().with_distances(100.0, 200.0);
        streaming.add_cell(Cell::new("gone", "nowhere/missing.scene", Vec3::ZERO, 1.0));

        streaming.update(&mut scene, Vec3::ZERO, None);
        assert_eq!(streaming.cell("gone").unwrap().state(), CellState::Failed);

        for _ in 0..10 {
            let report = streaming.update(&mut scene, Vec3::ZERO, None);
            assert!(!report.changed());
        }
    }

    #[test]
    fn a_failed_cell_can_be_retried_by_hand() {
        let directory = stage("retry");
        let mut scene = Scene::new();
        let mut streaming = Streaming::new().with_distances(100.0, 200.0);
        let path = directory.join("late.scene");
        streaming.add_cell(Cell::new("late", &path, Vec3::ZERO, 1.0));

        streaming.update(&mut scene, Vec3::ZERO, None);
        assert_eq!(streaming.cell("late").unwrap().state(), CellState::Failed);

        // 文件后来才就位——手动重试应当成功。
        write_cell(&directory, "late", |s| {
            s.add_node(Node::new("x").with_mesh(Mesh::cube()));
        });
        assert!(streaming.force_load(&mut scene, "late", None));
        assert!(streaming.cell("late").unwrap().is_loaded());

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn cells_can_be_driven_entirely_by_hand() {
        let directory = stage("manual");
        let path = write_cell(&directory, "a", |s| {
            s.add_node(Node::new("x").with_mesh(Mesh::cube()));
        });

        let mut scene = Scene::new();
        // 距离设成无穷大就等于「完全手动」。
        let mut streaming = Streaming::new().with_distances(f32::INFINITY, f32::INFINITY);
        streaming.add_cell(Cell::new("a", path, Vec3::ZERO, 1.0));

        assert!(streaming.force_load(&mut scene, "a", None));
        assert!(!streaming.force_load(&mut scene, "a", None), "重复装载该被忽略");
        assert_eq!(streaming.loaded_count(), 1);

        assert!(streaming.force_unload(&mut scene, "a"));
        assert!(!streaming.force_unload(&mut scene, "a"));
        assert_eq!(streaming.loaded_count(), 0);

        assert!(!streaming.force_load(&mut scene, "nonexistent", None));

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn unloading_removes_every_node_of_the_cell() {
        let directory = stage("cleanup");
        let path = write_cell(&directory, "a", |scene| {
            let parent = scene.add_node(Node::new("parent").with_mesh(Mesh::cube()));
            scene.add_node_with_parent(Node::new("child").with_mesh(Mesh::cube()), parent);
            scene.add_node_with_parent(Node::new("grandchild"), parent);
        });

        let mut scene = Scene::new();
        let before = scene.nodes().alive_count();

        let mut streaming = Streaming::new().with_distances(10.0, 20.0);
        streaming.add_cell(Cell::new("a", path, Vec3::ZERO, 1.0));
        streaming.update(&mut scene, Vec3::ZERO, None);
        assert!(scene.nodes().alive_count() > before);

        streaming.update(&mut scene, Vec3::X * 1000.0, None);

        // 只剩容器节点，区块的四个节点（含区块根）全走了。
        assert_eq!(scene.nodes().alive_count(), before + 1);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_cells_physics_objects_come_and_go_with_it() {
        let directory = stage("physics");
        let path = write_cell(&directory, "a", |scene| {
            scene.add_node(
                Node::new("ground")
                    .with_rigid_body(RigidBody::fixed())
                    .with_collider(Collider::cuboid(Vec3::splat(1.0))),
            );
        });

        let mut scene = Scene::new();
        let mut streaming = Streaming::new().with_distances(10.0, 20.0);
        streaming.add_cell(Cell::new("a", path, Vec3::ZERO, 1.0));

        streaming.update(&mut scene, Vec3::ZERO, None);
        scene.step_physics(1.0 / 60.0);
        assert_eq!(scene.physics().body_count(), 1, "区块的刚体没进物理世界");

        streaming.update(&mut scene, Vec3::X * 1000.0, None);
        scene.step_physics(1.0 / 60.0);
        assert_eq!(scene.physics().body_count(), 0, "卸载后留下了幽灵刚体");

        let _ = std::fs::remove_dir_all(&directory);
    }

    // ── `Scene::merge` 的句柄重映射 ──

    #[test]
    fn merging_remaps_the_hierarchy() {
        let mut source = Scene::new();
        let parent = source.add_node(Node::new("p").with_position(Vec3::Y));
        let child = source.add_node_with_parent(Node::new("c").with_position(Vec3::X), parent);
        assert_eq!(source[child].parent(), parent);

        // 目标场景先塞几个节点，逼得句柄不可能原样对上。
        let mut target = Scene::new();
        for index in 0..7 {
            target.add_node(Node::new(format!("filler{index}")));
        }

        let root = target.merge(source, target.root());
        target.update();

        let new_parent = target.find_by_name("p").unwrap();
        let new_child = target.find_by_name("c").unwrap();

        assert_ne!(new_child, child, "句柄居然没变，这个测试就白测了");
        assert_eq!(target[new_child].parent(), new_parent);
        assert!(target[new_parent].children().contains(&new_child));
        // 世界变换按新的层级重算出来，说明父子关系确实接对了。
        assert_eq!(
            target[new_child].global_position(),
            Vec3::new(1.0, 1.0, 0.0)
        );
        assert!(target[root].children().contains(&new_parent) || root == new_parent);
    }

    #[test]
    fn merging_remaps_skin_joints() {
        // 漏掉这一处的症状是「并进来之后角色跟着另一个物体动」，且不会报错。
        let mut source = Scene::new();
        let bone_a = source.add_node(Node::new("bone_a"));
        let bone_b = source.add_node(Node::new("bone_b"));
        source.add_node(
            Node::new("skinned")
                .with_mesh(Mesh::cube())
                .with_skin(Skin::new(
                    vec![bone_a, bone_b],
                    vec![Mat4::IDENTITY, Mat4::IDENTITY],
                )),
        );

        let mut target = Scene::new();
        for index in 0..11 {
            target.add_node(Node::new(format!("filler{index}")));
        }
        target.merge(source, target.root());

        let skinned = target.find_by_name("skinned").unwrap();
        let joints = target[skinned].skin().unwrap().joints().to_vec();

        assert_eq!(joints.len(), 2);
        assert_eq!(target[joints[0]].name, "bone_a");
        assert_eq!(target[joints[1]].name, "bone_b");
    }

    #[test]
    fn merging_remaps_joint_endpoints() {
        let mut source = Scene::new();
        let anchor = source.add_node(Node::new("anchor").with_rigid_body(RigidBody::fixed()));
        let bob = source.add_node(
            Node::new("bob")
                .with_position(Vec3::X * 2.0)
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.3)),
        );
        source.add_node(Node::new("joint").with_joint(crate::Joint::new(
            anchor,
            bob,
            kphysics::JointDesc::fixed(Vec3::ZERO, Vec3::NEG_X * 2.0),
        )));

        let mut target = Scene::new();
        for index in 0..13 {
            target.add_node(Node::new(format!("filler{index}")));
        }
        target.merge(source, target.root());

        let joint_node = target.find_by_name("joint").unwrap();
        let joint = target[joint_node].joint().unwrap();

        assert_eq!(target[joint.body1()].name, "anchor");
        assert_eq!(target[joint.body2()].name, "bob");

        // 接对了才建得出原生关节。
        target.step_physics(1.0 / 60.0);
        assert_eq!(target.physics().joint_count(), 1);
    }

    #[test]
    fn merging_keeps_the_targets_own_environment() {
        // 一个区块无权改写整张地图的天光。
        let mut source = Scene::new();
        source.environment_mut().intensity = 0.1;

        let mut target = Scene::new();
        target.environment_mut().intensity = 0.9;
        target.merge(source, target.root());

        assert_eq!(target.environment().intensity, 0.9);
    }

    #[test]
    fn merging_an_empty_scene_is_harmless() {
        let mut target = Scene::new();
        let before = target.nodes().alive_count();

        let root = target.merge(Scene::new(), target.root());

        // 空场景也有一个根节点，并进来就是一个空子树。
        assert!(root.is_some());
        assert_eq!(target.nodes().alive_count(), before + 1);
    }

    #[test]
    fn merged_transforms_stack_on_the_parent() {
        let mut source = Scene::new();
        source.add_node(Node::new("thing").with_position(Vec3::X));

        let mut target = Scene::new();
        let pivot = target.add_node(Node::new("pivot").with_position(Vec3::Y * 10.0));
        target.merge(source, pivot);
        target.update();

        let thing = target.find_by_name("thing").unwrap();
        assert_eq!(target[thing].global_position(), Vec3::new(1.0, 10.0, 0.0));
    }
}
