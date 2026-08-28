//! 脚本每帧的两笔开销：**一次 tick 的固定成本**，与**随脚本节点数增长的扫描**。
//!
//! 分开量是有原因的——它们的量级差着两个数量级，混在一起会互相掩盖：
//!
//! - 固定成本在 `HostGuard::park`：场景要整个搬进线程局部，调用方位置上得放个
//!   空壳顶着。这个空壳曾经**每次 park 现造、收场时析构**，而造一个 `Scene`
//!   意味着造一整个 rapier 物理世界（连造带拆实测 63 µs）。`kapp` 每帧至少
//!   park 两次（`process` + 物理子步的 `physics_process`），于是每帧凭空烧掉
//!   两百多微秒。改成空壳循环复用之后剩下几微秒。**`park_only` 就是守这条线的。**
//!
//! - 扫描在 `instantiate_pending`：每帧过一遍 `script_nodes()` 找还没实例化的
//!   槽位。已经跑起来的在三个布尔判断之后就跳过了，所以每个节点只有几纳秒。
//!   之所以还是要量，是因为它**随场景规模线性增长**，而且要改掉它就得引入
//!   脏标记协议（`ScriptSlot::enabled` 是个 pub 字段，游戏侧直接写，
//!   kscript 看不见）——值不值得做，得有数字才能谈。
//!
//! # 别忘了 README 里那条规矩
//!
//! 这台机器上两次运行之间能差两倍以上，所以这里**留了一条 `control/`**：
//! 它不碰脚本系统，同一轮里它涨了多少，就说明机器慢了多少。
//! 要下「脚本这边快了 / 慢了」的结论，先看它。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kengine::kasset::MemoryResourceIo;
use kengine::prelude::*;
use std::hint::black_box;
use std::sync::Arc;

/// 各档规模。跨两个数量级，好看出扫描是不是线性的。
const SIZES: [usize; 3] = [100, 2_000, 20_000];

const DT: f32 = 1.0 / 60.0;

/// 一个装好脚本、并且**已经加载完**的资源管理器。
///
/// 走内存 IO 而不是读磁盘：这里量的是运行时，不是文件系统。
/// 必须预热——脚本是异步加载的，不等它读完的话，bench 量到的是
/// 「每帧发现没就绪、直接返回」，那条路径快得没有意义。
fn resources(source: &str) -> ResourceManager {
    let mut io = MemoryResourceIo::new();
    io.add("s.js", source.as_bytes().to_vec());
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(ScriptLoader);
    let _ = manager.request_blocking::<Script>("s.js");
    manager
}

/// `count` 个挂着脚本的节点。
///
/// `enabled` 为 false 时脚本不会实例化，于是每帧只剩下那次扫描——
/// 这正是把「扫描」从「执行」里择出来的办法。
fn stage(count: usize, enabled: bool) -> Scene {
    let mut scene = Scene::new();
    for i in 0..count {
        let handle = scene.add_node(Node::new(format!("n{i}")).with_script("s.js"));
        if !enabled {
            scene[handle]
                .script_mut()
                .expect("刚挂上的脚本槽位")
                .enabled = false;
        }
    }
    scene.update();
    scene
}

fn script_tick(c: &mut Criterion) {
    let mut group = c.benchmark_group("script_tick");

    // ── 固定成本 ──
    // 空场景、零个脚本。量到的全是 park 的搬运：两个 `Scene`、一个 `Host`、
    // 一个 `Input` 进出线程局部。这条曾经是 113 µs。
    {
        let resources = resources("return {};");
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let mut input = Input::new();
        group.bench_function("park_only", |b| {
            b.iter(|| {
                black_box(runtime.process(black_box(&mut scene), &mut input, &resources, DT, 0.0))
                    .len()
            });
        });
    }

    // ── 扫描 ──
    // 节点都挂着脚本但全部停用：一个实例都不会建，每帧只有那趟遍历。
    for size in SIZES {
        let resources = resources("return {};");
        let mut runtime = ScriptRuntime::new();
        let mut scene = stage(size, false);
        let mut input = Input::new();

        group.bench_with_input(BenchmarkId::new("idle_slots", size), &size, |b, _| {
            b.iter(|| {
                black_box(runtime.process(black_box(&mut scene), &mut input, &resources, DT, 0.0))
                    .len()
            });
        });
    }

    // ── 空回调 ──
    // 每个实例都调一次 `_process`，但方法体是空的。和 `idle_slots` 的差值
    // 就是「查到方法、进 VM、出来」的固定价钱，不含任何引擎 API 调用。
    for size in SIZES {
        let resources = resources("return { _process() {} };");
        let mut runtime = ScriptRuntime::new();
        let mut scene = stage(size, true);
        let mut input = Input::new();
        runtime.process(&mut scene, &mut input, &resources, DT, 0.0);

        group.bench_with_input(BenchmarkId::new("empty_callback", size), &size, |b, _| {
            b.iter(|| {
                black_box(runtime.process(black_box(&mut scene), &mut input, &resources, DT, 0.0))
                    .len()
            });
        });
    }

    // ── 真跑 ──
    // 方法体里做一次 `self.position.y += dt`——最普通不过的一行脚本。
    // 和 `empty_callback` 的差值全是这一行的价钱：`self` 建一个 `Node`、
    // `.position` 建一个 `BoundVector3`、读一次写一次穿过桥。
    // 下面的 `raw_bridge` 做同一件事但不建这两个对象，两者一比就知道
    // 包装层占了多少。
    for size in SIZES {
        let resources = resources("return { _process(dt) { self.position.y += dt; } };");
        let mut runtime = ScriptRuntime::new();
        let mut scene = stage(size, true);
        let mut input = Input::new();

        // 先跑一帧把实例都建起来：实例化只发生一次，混进采样里会
        // 让第一批样本高得离谱，criterion 会把它当成离群点报警。
        runtime.process(&mut scene, &mut input, &resources, DT, 0.0);

        group.bench_with_input(BenchmarkId::new("running", size), &size, |b, _| {
            b.iter(|| {
                black_box(runtime.process(black_box(&mut scene), &mut input, &resources, DT, 0.0))
                    .len()
            });
        });
    }

    // ── 包装层的价钱 ──
    //
    // 和 `running` 做的是**同一件事**（把一个数写进 position.y），区别只在
    // 绕不绕 `prelude.js` 那层包装：这一档直接捅桥，不建 `Node`、
    // 不建 `BoundVector3`。
    //
    // 两条的差值就是包装层的全部成本。放在同一个 group 里是有意的——
    // 这台机器两次运行之间能差两倍以上（见 README），只有同一轮里量出来的
    // 差值才说明得了问题。
    for size in SIZES {
        let resources =
            resources("return { _process(dt) { __k.setComponent(__k.selfId(), 0, 1, dt); } };");
        let mut runtime = ScriptRuntime::new();
        let mut scene = stage(size, true);
        let mut input = Input::new();
        runtime.process(&mut scene, &mut input, &resources, DT, 0.0);

        group.bench_with_input(BenchmarkId::new("raw_bridge", size), &size, |b, _| {
            b.iter(|| {
                black_box(runtime.process(black_box(&mut scene), &mut input, &resources, DT, 0.0))
                    .len()
            });
        });
    }

    group.finish();
}

/// 对照组：一段完全不碰脚本系统的工作。
///
/// 同一轮里它涨了多少，机器就慢了多少。没有它的话，
/// 「脚本 tick 慢了 40%」这种话没法判断是代码的事还是机器的事。
fn control(c: &mut Criterion) {
    let mut scene = Scene::new();
    let mesh = Mesh::cube();
    for i in 0..2_000 {
        scene.add_node(
            Node::new(format!("n{i}"))
                .with_mesh(mesh.clone())
                .with_position(Vec3::new(i as f32, 0.0, 0.0)),
        );
    }

    c.bench_function("control/scene_update/2000", |b| {
        b.iter(|| {
            black_box(&mut scene).update();
        });
    });
}

criterion_group!(benches, script_tick, control);
criterion_main!(benches);
