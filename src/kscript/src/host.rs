//! 宿主：让原生函数拿到**真正的** `&mut Scene`。
//!
//! # 怎么做到的（而且不用 `unsafe`）
//!
//! boa 的原生函数必须是 `'static`，没法捕获 `&mut Scene`。常见解法是线程局部
//! **裸指针**（`unsafe`，别名不变量全靠约定维持）。这里换一招：tick 开始时把
//! 场景连同其它宿主状态**整个搬进**线程局部，跑完再搬回去。
//!
//! 搬运的是结构体本身（堆上的节点池、物理世界一个字节都不动），
//! 一来一回两次 memcpy，纳秒级。换来的是**零 `unsafe`** 的实时访问：
//! 脚本读到的是此刻的场景，写下去立刻生效，还能当场打射线。
//!
//! # 三条不变量
//!
//! 1. **搬回去靠 `Drop`**：脚本抛异常时中间的代码会被跳过，手动搬回的话
//!    场景就永远留在线程局部里了——调用方拿到个空壳，整个游戏静默失效。
//! 2. **借用期间不碰 VM**：[`with_host`] 的闭包拿不到 `Context`，
//!    所以不可能在持有借用时回调进 JS。这是**结构上**堵死的，不是靠自觉。
//!    万一真发生重入，`try_borrow_mut` 返回 `None` 而不是 panic。
//! 3. **状态属于运行时，不属于线程**：节点登记处随 [`Host`] 一起进出。
//!    放在线程局部里长住的话，同一条线程上的两个运行时会互相看到对方的句柄——
//!    这个 bug 在并行跑测试时原形毕露过一次。

use crate::runtime::Signal;
use kcore::pool::Handle;
use kinput::Input;
use kscene::{Node, Scene};
use std::cell::RefCell;

thread_local! {
    /// tick 期间寄存在这里的宿主状态。
    static PARKED: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// 节点句柄登记处：JS 侧只拿到小整数下标。
///
/// 为什么不直接把 `Handle` 传过去：它是 index + generation 共 64 位，
/// 而 JS 的 `f64` 只有 53 位整数精度，硬塞会在世代号涨上去之后悄悄错位。
/// 换成下标既精确，又天然把脚本能碰的节点限制在引擎给过的范围内。
#[derive(Default)]
pub(crate) struct Registry {
    handles: Vec<Handle<Node>>,
    lookup: fxhash::FxHashMap<Handle<Node>, u32>,
}

impl Registry {
    pub(crate) fn id_of(&mut self, handle: Handle<Node>) -> u32 {
        if let Some(id) = self.lookup.get(&handle) {
            return *id;
        }
        let id = self.handles.len() as u32;
        self.handles.push(handle);
        self.lookup.insert(handle, id);
        id
    }

    fn handle_of(&self, id: u32) -> Option<Handle<Node>> {
        self.handles.get(id as usize).copied()
    }
}

/// 一个节点原型：游戏侧登记，脚本按名字 `spawn` 出来。
///
/// 脚本不该认识 `Mesh` 与材质——那等于把整个渲染栈拖进 JS 层，
/// 而且换一套美术资源就要改脚本。名字是两边唯一的约定。
pub(crate) type Prototype = Box<dyn Fn() -> Node>;

/// tick 期间原生函数能碰到的一切。
#[derive(Default)]
pub(crate) struct Host {
    pub(crate) scene: Option<Scene>,
    /// 本帧的输入。和场景一样是搬进来的，跑完搬回去。
    pub(crate) input: Option<Input>,
    pub(crate) registry: Registry,
    /// 可供 `spawn` 的原型，按名字取。
    ///
    /// 属于运行时而不是某一次 tick：随 [`Host`] 一起进出线程局部，
    /// 注册一次之后一直在。
    pub(crate) prototypes: fxhash::FxHashMap<String, Prototype>,
    /// 当前正在跑的脚本挂在哪个节点上，`self` 读它。
    pub(crate) current: Handle<Node>,
    /// 本次 tick 已经生成了多少个节点。
    pub(crate) spawned: usize,
    pub(crate) dt: f32,
    pub(crate) elapsed: f32,
    pub(crate) signals: Vec<Signal>,
}

/// 一帧最多接受多少个信号。
///
/// 防的是写错的脚本：`for(;;) emit('x')` 会在循环上限触发之前先把内存吃光。
pub(crate) const MAX_SIGNALS: usize = 4096;

/// 一次 tick 最多生成多少个节点。
///
/// 同一个道理，只是更凶：`for(;;) spawn('Enemy')` 每次都会往场景池里塞一个
/// 带网格带物理的节点，比攒信号吃内存快得多。
pub(crate) const MAX_SPAWNS: usize = 1024;

/// 宿主状态的寄存凭证。析构时把一切搬回原处。
pub(crate) struct HostGuard<'a> {
    owner_scene: &'a mut Scene,
    owner_input: &'a mut Input,
    owner_host: &'a mut Host,
    /// 空壳的家。tick 期间是 [`None`]——那个空壳正占着调用方的位置。
    owner_spare: &'a mut Option<Scene>,
}

impl Drop for HostGuard<'_> {
    fn drop(&mut self) {
        let Some(mut parked) = PARKED.with(|slot| slot.borrow_mut().take()) else {
            return;
        };
        if let Some(real_scene) = parked.scene.take() {
            // 真场景回到调用方手里，换出来的空壳收回 `spare` 等下一次用。
            // 这里要是让它落地析构，就等于每帧拆掉一个物理世界再建一个。
            let empty = std::mem::replace(self.owner_scene, real_scene);
            *self.owner_spare = Some(empty);
        }
        if let Some(mut input) = parked.input.take() {
            std::mem::swap(self.owner_input, &mut input);
        }
        // 登记处、原型表与累计的信号都还给运行时。
        std::mem::swap(self.owner_host, &mut parked);
    }
}

impl<'a> HostGuard<'a> {
    /// 把场景、输入与宿主状态寄存进线程局部。
    ///
    /// # 那个空壳
    ///
    /// 场景要整个搬进线程局部，调用方的位置上就得放点东西顶着。现造一个
    /// `Scene` 要新建一整个 rapier 物理世界，连造带拆实测 **63 µs**——
    /// 每帧两三次 park 就是每帧一大截白烧。
    ///
    /// 所以空壳**只造一次**，之后在 `spare` 与调用方的位置之间来回倒：
    /// park 时从 `spare` 取出来顶上，[`Drop`] 时再收回 `spare`。稳态下
    /// 这条路径上一次分配都没有，只剩几次结构体 memcpy。
    ///
    /// 输入不需要这份小心：`Input::default()` 只是几个空表，直接 `take` 即可。
    pub(crate) fn park(
        scene: &'a mut Scene,
        input: &'a mut Input,
        host: &'a mut Host,
        spare: &'a mut Option<Scene>,
        dt: f32,
        elapsed: f32,
    ) -> Self {
        // 取不出空壳只可能是第一次 park（或者上一个凭证被 forget 了），
        // 那就现造一个——这是整条路径上唯一会分配的地方。
        let empty = spare.take().unwrap_or_default();
        let real_scene = std::mem::replace(scene, empty);

        let mut parked = std::mem::take(host);
        parked.scene = Some(real_scene);
        parked.input = Some(std::mem::take(input));
        parked.current = Handle::NONE;
        parked.spawned = 0;
        parked.dt = dt;
        parked.elapsed = elapsed;
        parked.signals.clear();

        PARKED.with(|slot| *slot.borrow_mut() = Some(parked));

        Self {
            owner_scene: scene,
            owner_input: input,
            owner_host: host,
            owner_spare: spare,
        }
    }
}

/// 借用寄存中的宿主状态。
///
/// 不在 tick 期间、或者发生重入时返回 [`None`]——调用方一律当成
/// 「这次操作没做成」，而不是 panic。脚本能触发的路径不该让引擎崩。
pub(crate) fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    PARKED.with(|slot| {
        let mut guard = slot.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

/// 借用寄存中的场景。
pub(crate) fn with_scene<R>(f: impl FnOnce(&mut Scene) -> R) -> Option<R> {
    with_host(|host| host.scene.as_mut().map(f)).flatten()
}

/// 借用寄存中的输入。
pub(crate) fn with_input<R>(f: impl FnOnce(&Input) -> R) -> Option<R> {
    with_host(|host| host.input.as_ref().map(f)).flatten()
}

/// 登记一个句柄，拿到给 JS 用的下标。
pub(crate) fn id_of(handle: Handle<Node>) -> f64 {
    with_host(|host| host.registry.id_of(handle) as f64).unwrap_or(-1.0)
}

/// 把 JS 传来的下标还原成句柄。
pub(crate) fn handle_of(id: f64) -> Option<Handle<Node>> {
    if !id.is_finite() || id < 0.0 {
        return None;
    }
    with_host(|host| host.registry.handle_of(id as u32)).flatten()
}

#[cfg(test)]
mod test {
    use super::*;
    use kmath::Vec3;

    /// 建一套「场景 + 输入 + 宿主 + 空壳」。
    fn stage() -> (Scene, Input, Host, Option<Scene>) {
        (
            Scene::new(),
            Input::new(),
            Host::default(),
            Some(Scene::new()),
        )
    }

    #[test]
    fn a_parked_scene_can_be_read_and_written_live() {
        // 整套方案的核心：写下去立刻生效，不是攒到帧末。
        let (mut scene, mut input, mut host, mut spare) = stage();
        let node = scene.add_node(Node::new("probe"));

        {
            let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.016, 1.0);
            with_scene(|live| live[node].transform.position = Vec3::Y * 42.0).unwrap();
            let read = with_scene(|live| live[node].transform.position).unwrap();
            assert_eq!(read, Vec3::Y * 42.0, "同一次 tick 里没读到刚写的值");
        }

        assert_eq!(
            scene[node].transform.position,
            Vec3::Y * 42.0,
            "改动没搬回来"
        );
    }

    #[test]
    fn the_empty_shell_is_recycled_not_rebuilt() {
        // 空壳要是每次 park 都现造一个、收场时又析构掉，那就是每帧连造带拆
        // 一整个 rapier 物理世界——实测 63 µs，一帧两三次 park 就很可观。
        //
        // 直接量时间的断言在 CI 上必然会闪，所以改成给空壳做个记号：
        // 它要是被换成新造的，记号就没了。
        let (mut scene, mut input, mut host, mut spare) = stage();
        let mark = spare
            .as_mut()
            .expect("空壳")
            .add_node(Node::new("shell-marker"));

        for _ in 0..3 {
            let guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.0, 0.0);
            drop(guard);
        }

        let shell = spare.as_ref().expect("空壳没还回来");
        assert_eq!(shell[mark].name, "shell-marker", "空壳被重造了");
    }

    #[test]
    fn everything_comes_back_even_if_the_tick_panics() {
        // 手动搬回的话，脚本一抛异常场景就永远留在线程局部里，
        // 调用方拿到个空壳——整个游戏静默失效，比崩溃还难查。
        let (mut scene, mut input, mut host, mut spare) = stage();
        let node = scene.add_node(Node::new("survivor"));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.016, 0.0);
            panic!("脚本炸了");
        }));

        assert!(result.is_err());
        assert_eq!(scene[node].name, "survivor", "场景没搬回来");
        assert!(with_scene(|_| ()).is_none(), "线程局部该被清空");
    }

    #[test]
    fn the_registry_belongs_to_the_runtime_not_the_thread() {
        // 登记处留在线程局部长住的话，同一条线程上的两个运行时会互相看到
        // 对方的句柄——并行跑测试时这个 bug 原形毕露过一次。
        let (mut scene_a, mut input_a, mut host_a, mut spare_a) = stage();
        let node_a = scene_a.add_node(Node::new("a"));
        let id_a = {
            let _guard = HostGuard::park(
                &mut scene_a,
                &mut input_a,
                &mut host_a,
                &mut spare_a,
                0.0,
                0.0,
            );
            id_of(node_a)
        };

        // 另一套完全独立的宿主，登记处应当从零开始。
        let (mut scene_b, mut input_b, mut host_b, mut spare_b) = stage();
        scene_b.add_node(Node::new("filler"));
        let node_b = scene_b.add_node(Node::new("b"));
        let (id_b, resolved) = {
            let _guard = HostGuard::park(
                &mut scene_b,
                &mut input_b,
                &mut host_b,
                &mut spare_b,
                0.0,
                0.0,
            );
            (id_of(node_b), handle_of(id_of(node_b)))
        };

        assert_eq!(id_a, 0.0);
        assert_eq!(id_b, 0.0, "第二个运行时的登记处该从零开始");
        assert_eq!(resolved, Some(node_b), "下标解回了别人的句柄");
    }

    #[test]
    fn the_registry_survives_between_ticks_of_the_same_runtime() {
        // 同一个运行时里，脚本上一帧拿到的下标这一帧还得管用。
        let (mut scene, mut input, mut host, mut spare) = stage();
        let node = scene.add_node(Node::new("stable"));

        let first = {
            let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.0, 0.0);
            id_of(node)
        };
        let (second, resolved) = {
            let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.0, 0.0);
            (id_of(node), handle_of(first))
        };

        assert_eq!(first, second, "同一个句柄两次登记该拿到同一个下标");
        assert_eq!(resolved, Some(node));
    }

    #[test]
    fn a_bogus_id_resolves_to_nothing() {
        // 脚本可以往里塞任何数字，编不出一个指向任意内存的句柄才是关键。
        let (mut scene, mut input, mut host, mut spare) = stage();
        let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.0, 0.0);

        for bad in [-1.0, f64::NAN, f64::INFINITY, 1e30, 999.0] {
            assert_eq!(handle_of(bad), None, "{bad} 竟然解出了句柄");
        }
    }

    #[test]
    fn nothing_is_reachable_outside_a_tick() {
        assert!(with_scene(|_| 1).is_none());
        assert!(handle_of(0.0).is_none());
    }

    #[test]
    fn re_entrancy_is_refused_instead_of_panicking() {
        // 借用中再借用：`RefCell` 默认会 panic，这里必须降级成「没做成」。
        let (mut scene, mut input, mut host, mut spare) = stage();
        let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.0, 0.0);

        let inner = with_host(|_outer| with_host(|_| 1)).unwrap();

        assert!(inner.is_none(), "重入没被拦下");
    }

    #[test]
    fn timing_is_visible_to_the_bridge() {
        let (mut scene, mut input, mut host, mut spare) = stage();
        let _guard = HostGuard::park(&mut scene, &mut input, &mut host, &mut spare, 0.25, 7.5);

        let (dt, elapsed) = with_host(|host| (host.dt, host.elapsed)).unwrap();

        assert_eq!(dt, 0.25);
        assert_eq!(elapsed, 7.5);
    }
}
