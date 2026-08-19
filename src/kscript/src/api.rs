//! 脚本与引擎之间的数据契约：快照进、命令出。
//!
//! # 为什么不让脚本直接访问场景
//!
//! boa 的原生函数必须是 `'static`，把 `&mut Scene` 递进 VM 只有两条路：
//! 线程局部裸指针（`unsafe`，别名不变量全靠约定维持），或者把整个 `Scene`
//! 包进 `Rc<RefCell>`（要改掉引擎的所有权结构，波及所有已有代码）。
//!
//! 这里走第三条：**tick 开始时把脚本关心的状态拍成快照递进去，脚本产生一串
//! 命令，返回后由引擎依次落地。** 全是安全 Rust，没有裸指针，
//! 而且整条链路能在没有 VM 的情况下逐条测试（本模块的测试就是这么写的）。
//!
//! 代价要说清楚：脚本读到的是**本 tick 开始时**的状态，写入在返回后生效。
//! 对逐帧游戏逻辑这恰好是想要的语义——所有脚本看到同一帧，谁先跑谁后跑
//! 不影响结果。真正的损失是**即时查询**：脚本没法在中途打一条射线然后
//! 根据结果决定下一步，只能把查询排到下一帧。

use kmath::{Quat, Vec3};

/// 快照里一个节点的编号。
///
/// **不是** `kcore` 的 `Handle`：那是 index + generation 共 64 位，
/// 塞不进 JS 的 `f64`（只有 53 位整数精度）。这里改成快照数组的下标，
/// 小整数、能精确表示，而且天然把脚本能碰的节点限制在快照范围内——
/// 脚本编不出一个指向任意内存的编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeRef(pub u32);

impl NodeRef {
    /// 无效引用。JS 里查不到节点时返回它。
    pub const NONE: Self = Self(u32::MAX);

    /// 是否有效。
    pub fn is_some(self) -> bool {
        self != Self::NONE
    }

    /// 从 JS 传来的数字还原。非整数、负数、超范围都会得到 [`NodeRef::NONE`]。
    pub fn from_js(value: f64) -> Self {
        if !value.is_finite() || value < 0.0 || value >= u32::MAX as f64 {
            return Self::NONE;
        }
        Self(value as u32)
    }

    /// 转成 JS 用的数字。
    pub fn to_js(self) -> f64 {
        self.0 as f64
    }
}

/// 快照里一个节点的状态。
#[derive(Debug, Clone, PartialEq)]
pub struct NodeState {
    /// 节点名。
    pub name: String,
    /// 局部位置。
    pub position: Vec3,
    /// 局部旋转。
    pub rotation: Quat,
    /// 局部缩放。
    pub scale: Vec3,
    /// 世界空间位置，由上一次 `update` 算出。
    pub world_position: Vec3,
    /// 是否可见。
    pub visible: bool,
}

impl Default for NodeState {
    fn default() -> Self {
        Self {
            name: String::new(),
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            world_position: Vec3::ZERO,
            visible: true,
        }
    }
}

/// 递给脚本的一帧快照。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    nodes: Vec<NodeState>,
    /// 距上一帧的秒数。
    pub dt: f32,
    /// 引擎启动至今的秒数。
    pub elapsed: f32,
}

impl Snapshot {
    /// 空快照。
    pub fn new(dt: f32, elapsed: f32) -> Self {
        Self {
            nodes: Vec::new(),
            dt,
            elapsed,
        }
    }

    /// 加一个节点，返回它在快照里的编号。
    pub fn push(&mut self, state: NodeState) -> NodeRef {
        self.nodes.push(state);
        NodeRef(self.nodes.len() as u32 - 1)
    }

    /// 节点数量。
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// 是否没有任何节点。
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 按编号取状态。越界返回 [`None`]。
    pub fn node(&self, node: NodeRef) -> Option<&NodeState> {
        self.nodes.get(node.0 as usize)
    }

    /// 按名字找节点。同名时返回第一个。
    pub fn find(&self, name: &str) -> NodeRef {
        self.nodes
            .iter()
            .position(|state| state.name == name)
            .map(|index| NodeRef(index as u32))
            .unwrap_or(NodeRef::NONE)
    }

    /// 全部节点状态。
    pub fn nodes(&self) -> &[NodeState] {
        &self.nodes
    }

    /// 清空节点，保留时间信息。复用快照可以省掉每帧的分配。
    pub fn clear(&mut self) {
        self.nodes.clear();
    }
}

/// 脚本对引擎发出的一条指令。
///
/// 刻意做成**粗粒度**：一条命令等于一次有意义的操作，而不是「设置某个字段」。
/// 细粒度的话，脚本改一个位置要发三条命令（x/y/z），既慢又难以校验。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 设置局部位置。
    SetPosition(NodeRef, Vec3),
    /// 在局部空间平移。
    Translate(NodeRef, Vec3),
    /// 设置局部旋转。
    SetRotation(NodeRef, Quat),
    /// 绕自身 Y 轴旋转（弧度）。
    RotateY(NodeRef, f32),
    /// 设置局部缩放。
    SetScale(NodeRef, Vec3),
    /// 设置可见性。
    SetVisible(NodeRef, bool),
    /// 给刚体施加冲量。节点没有刚体时忽略。
    ApplyImpulse(NodeRef, Vec3),
    /// 播放（或重放）节点上的声源。
    PlaySound(NodeRef),
    /// 删除节点及其子树。
    Despawn(NodeRef),
    /// 往日志里写一行。
    Log(String),
    /// 抛一个事件给游戏侧。脚本与 Rust 之间的单向消息。
    Emit {
        /// 事件名。
        name: String,
        /// 附带的数值。
        value: f64,
        /// 发出它的脚本挂在哪个节点上。
        ///
        /// 在**发出的那一刻**记下来。命令是攒到一帧末才一起回到 Rust 侧的，
        /// 那时早就不知道当时在跑的是谁了——事后再猜只能猜错。
        source: NodeRef,
    },
}

impl Command {
    /// 这条命令作用在哪个节点上；不针对节点的命令返回 [`None`]。
    pub fn target(&self) -> Option<NodeRef> {
        match self {
            Self::SetPosition(node, _)
            | Self::Translate(node, _)
            | Self::SetRotation(node, _)
            | Self::RotateY(node, _)
            | Self::SetScale(node, _)
            | Self::SetVisible(node, _)
            | Self::ApplyImpulse(node, _)
            | Self::PlaySound(node)
            | Self::Despawn(node) => Some(*node),
            Self::Log(_) | Self::Emit { .. } => None,
        }
    }
}

/// 脚本这一帧发出的命令。
#[derive(Debug, Clone, Default)]
pub struct CommandBuffer {
    commands: Vec<Command>,
    /// 因为超出上限而被丢掉的命令数。
    dropped: usize,
    /// 一帧最多接受多少条命令。
    limit: usize,
}

impl CommandBuffer {
    /// 默认的每帧命令上限。
    ///
    /// 上限存在的理由是**一个写错的脚本不该拖垮整个引擎**：
    /// `for(;;) engine.log('x')` 会在循环上限之前先撑爆内存。
    /// 超出后丢弃并计数，下一帧照常运行——比整个进程 OOM 强。
    pub const DEFAULT_LIMIT: usize = 4096;

    /// 建一个用默认上限的缓冲。
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            dropped: 0,
            limit: Self::DEFAULT_LIMIT,
        }
    }

    /// 指定上限。
    pub fn with_limit(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            ..Self::new()
        }
    }

    /// 追加一条命令。超出上限时丢弃并计数，返回是否收下了。
    pub fn push(&mut self, command: Command) -> bool {
        if self.commands.len() >= self.limit {
            self.dropped += 1;
            return false;
        }
        self.commands.push(command);
        true
    }

    /// 已收下的命令。
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// 命令数量。
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// 是否一条命令都没有。
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// 这一帧被丢掉了多少条。
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// 取走全部命令，缓冲复位。
    pub fn take(&mut self) -> Vec<Command> {
        self.dropped = 0;
        std::mem::take(&mut self.commands)
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.commands.clear();
        self.dropped = 0;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_node_ref_survives_a_trip_through_a_javascript_number() {
        for raw in [0u32, 1, 12345, 1 << 24] {
            assert_eq!(NodeRef::from_js(NodeRef(raw).to_js()), NodeRef(raw));
        }
    }

    #[test]
    fn a_bogus_number_becomes_a_none_ref_instead_of_a_wild_index() {
        // 脚本可以往里塞任何东西，编不出一个指向任意内存的编号才是关键。
        for bad in [-1.0, f64::NAN, f64::INFINITY, 1e30] {
            assert_eq!(NodeRef::from_js(bad), NodeRef::NONE);
        }
        assert!(!NodeRef::NONE.is_some());
    }

    #[test]
    fn a_fractional_number_truncates_rather_than_erroring() {
        // JS 里没有整数类型，`i + 0.5` 这种笔误应当落到某个确定的编号上，
        // 而不是让整个脚本崩掉。
        assert_eq!(NodeRef::from_js(3.7), NodeRef(3));
    }

    #[test]
    fn a_snapshot_hands_out_sequential_refs() {
        let mut snapshot = Snapshot::new(0.016, 1.0);
        let first = snapshot.push(NodeState {
            name: "a".into(),
            ..Default::default()
        });
        let second = snapshot.push(NodeState {
            name: "b".into(),
            ..Default::default()
        });

        assert_eq!(first, NodeRef(0));
        assert_eq!(second, NodeRef(1));
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.node(first).unwrap().name, "a");
    }

    #[test]
    fn an_out_of_range_ref_reads_back_as_nothing() {
        let snapshot = Snapshot::new(0.0, 0.0);

        assert!(snapshot.node(NodeRef(0)).is_none());
        assert!(snapshot.node(NodeRef::NONE).is_none());
    }

    #[test]
    fn nodes_can_be_found_by_name() {
        let mut snapshot = Snapshot::new(0.0, 0.0);
        snapshot.push(NodeState {
            name: "player".into(),
            ..Default::default()
        });
        snapshot.push(NodeState {
            name: "enemy".into(),
            ..Default::default()
        });

        assert_eq!(snapshot.find("enemy"), NodeRef(1));
        assert_eq!(snapshot.find("nobody"), NodeRef::NONE);
    }

    #[test]
    fn clearing_a_snapshot_keeps_the_timing() {
        let mut snapshot = Snapshot::new(0.016, 5.0);
        snapshot.push(NodeState::default());

        snapshot.clear();

        assert!(snapshot.is_empty());
        assert_eq!(snapshot.dt, 0.016);
        assert_eq!(snapshot.elapsed, 5.0);
    }

    #[test]
    fn commands_report_the_node_they_act_on() {
        let node = NodeRef(3);

        assert_eq!(Command::RotateY(node, 1.0).target(), Some(node));
        assert_eq!(Command::Despawn(node).target(), Some(node));
        assert_eq!(Command::Log("hi".into()).target(), None);
        assert_eq!(
            Command::Emit {
                name: "hit".into(),
                value: 1.0,
                source: NodeRef(7),
            }
            .target(),
            None,
            "事件不作用在节点上，来源只是元信息"
        );
    }

    #[test]
    fn a_command_buffer_collects_in_order() {
        let mut buffer = CommandBuffer::new();
        buffer.push(Command::Log("first".into()));
        buffer.push(Command::Log("second".into()));

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.commands()[0], Command::Log("first".into()));
    }

    #[test]
    fn a_runaway_script_is_capped_rather_than_exhausting_memory() {
        // `for(;;) engine.log('x')` 会在循环上限之前先撑爆内存。
        let mut buffer = CommandBuffer::with_limit(3);
        for index in 0..100 {
            buffer.push(Command::Log(format!("{index}")));
        }

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped(), 97);
    }

    #[test]
    fn taking_the_commands_resets_the_buffer() {
        let mut buffer = CommandBuffer::with_limit(1);
        buffer.push(Command::Log("a".into()));
        buffer.push(Command::Log("b".into()));
        assert_eq!(buffer.dropped(), 1);

        let taken = buffer.take();

        assert_eq!(taken.len(), 1);
        assert!(buffer.is_empty());
        assert_eq!(buffer.dropped(), 0, "取走之后计数该复位");
    }

    #[test]
    fn a_zero_limit_still_accepts_one_command() {
        // 上限为 0 会让脚本完全失声，多半是调用方笔误——夹到 1 更接近本意。
        let mut buffer = CommandBuffer::with_limit(0);

        assert!(buffer.push(Command::Log("x".into())));
    }
}
