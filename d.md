# d.md —— 对象驱动 demo 的执行计划

## 一、你的引擎是不是这个架构

**是，而且已经跑起来了。** 对照你画的那张图：

```
Engine Core (Rust)      →  29 个 k* crate：krender / kphysics / kscene / …
JS Binding / FFI        →  kscript::bridge（扁平原生函数 __k.*）
                           kscript::host（把 Scene 整个搬进线程局部，零 unsafe）
JavaScript Runtime      →  kscript::runtime（boa_engine 0.21）
Game Scripts            →  prelude.js 把桥包成 GDScript 手感：self / Node / Vector3
Player / Enemy / …      →  节点上的 ScriptSlot，一个节点一个脚本实例
```

`Scene → Node → Script` 这条链是现成的：`kscene::Node` 上有 `ScriptSlot`
（只存路径，所以能随场景存档），`kscene` 完全不认识 boa；`ScriptRuntime`
每帧 `_process`、每物理子步 `_physics_process`，还带热重载、`_save`/`_load`
存档、`emit` 信号回 Rust、`require` 模块系统、出错自动停用 + 首错留存。

比图上多的：脚本能**当场读写场景、当场打射线**（`raycast` 即时返回），
不是「快照进、命令出」。这一条是 `examples/script_hotreload.rs` 已经在演示的。

## 二、要做 Player / Enemy / Inventory，缺三样

demo 逼出来的三个真实缺口（不是为了写 demo 硬造的需求）：

1. **脚本读不到输入。** 桥里没有任何输入函数。Player 脚本连走路都做不到，
   只能由 Rust 侧代劳——那就不叫脚本驱动了。
2. **脚本生不出节点。** 只有 `queueFree` 删，没有生。敌人刷新、子弹、掉落物
   全都做不了。这是「对象驱动」最核心的一半。
3. **脚本之间说不上话。** 现在只有 `emit`（JS → Rust 单向数值）。
   GDScript 里的 `get_node("Inventory").add_item(...)` 在这儿写不出来，
   Inventory 这种「一个对象持有状态、别人调它的方法」的模式就无从谈起。

结论：这三样都得补，属于**重大修改**，所以有这份计划。

## 三、执行计划

### 步骤 1 —— 输入进桥（kscript）

沿用 host 现成的「搬进线程局部」手法，`Input` 和 `Scene` 一起 park
（`Input: Default`，两次 memcpy，零 unsafe，与现有三条不变量一致）。

- `host.rs`：`Host` 加 `input: Option<Input>`；`HostGuard::park` 多收一个
  `&mut Input` 与一个 `spare_input`；加 `with_input`。
- `bridge.rs`：`actionPressed / actionJustPressed / actionJustReleased /
  axis / axisVector / mousePosition / mouseDelta / mouseButton(name) /
  mouseButtonJustPressed(name)`。
- `prelude.js`：包成 `Input.pressed("jump")` / `Input.axis("move_x")` /
  `Input.axisVector("move_x","move_z")` / `Input.mouse` 等。

**只暴露 action / axis，不暴露具体键位。** 键位绑定留在 Rust 侧的
`Bindings`——脚本里硬编码 `KeyCode` 会让改键功能永远做不了，
而 kinput 的整套设计就是为了避免这件事。

- 签名变化：`ScriptRuntime::process(scene, input, resources, dt, elapsed)`、
  `physics_process(scene, input, dt, elapsed)`。改 8 处测试调用点 + kapp 2 处。

### 步骤 2 —— 生成节点：原型注册表（kscript + kapp）

脚本不该认识 `Mesh`、`PbrMaterial`——那会把整个渲染栈拖进 JS 层。
改成**游戏侧注册原型、脚本按名字生成**：

```rust
App::new().with_prototype("Enemy", || {
    Node::new("Enemy").with_mesh(Mesh::cube()).with_script("…/enemy.js")
})
```

```js
const e = spawn("Enemy", new Vector3(x, 0.5, z));   // 返回 Node，立刻可用
```

- `Host` 加 `prototypes: FxHashMap<String, Box<dyn Fn() -> Node>>`
  （随 Host 一起 take/swap，闭包 `'static`）。
- `ScriptRuntime::register_prototype(name, f)`；`App::with_prototype` 转发。
- 桥 `spawn(name, x, y, z)`：取原型 → 设位置 → `scene.add_node`
  （它会就地登记物理索引，所以带碰撞体的原型当帧就进物理世界）→ 返回登记下标。
- 一帧的生成数量设上限（同 `MAX_SIGNALS` 的思路），防写错的脚本把内存吃光。
- 新生成节点若带脚本，下一帧由 `instantiate_pending` 实例化——晚一帧，可接受，
  在文档里写明。

### 步骤 3 —— 脚本间调用：`node.script`（kscript）

**不加 Rust 桥**——桥的不变量 2 是「借用期间不碰 VM」，
在原生函数里回调 JS 正好违反它。改成纯 JS 侧的实例登记表：

- `runtime` 实例化成功后，把实例对象写进全局 `__instances[nodeId]`
  （`nodeId` 取自 `host.registry`，与桥用的是同一套下标）；实例回收时删掉。
- `prelude.js`：`get script() { return __instances[this._id] ?? null; }`。

于是：

```js
getNode("Inventory").script.add("coin", 1);
target.script.hit(25);
```

零 Rust 代码、零重入风险，正好是「对象驱动」要的那个手感。

### 步骤 4 —— demo（examples/kengine/demo/）

一个能玩的小竞技场，四类对象各自由脚本驱动：

```
main.rs            场景、键位绑定、原型注册、HUD（kui）
scripts/player.js      读 axis 移动 + 转向；攻击生成子弹；hp / damage()
scripts/enemy.js       追玩家；hit(dmg) 被子弹调用；死亡时 spawn("Coin") 并 emit
scripts/bullet.js      前进 + 命中检测 + 超时自毁；调 enemy.script.hit()
scripts/coin.js        旋转浮动；靠近玩家时调 inventory.script.add()
scripts/inventory.js   纯数据对象：items / add / count；_save + _load 演示存档
scripts/spawner.js     定时 spawn("Enemy")，并持有敌人列表供子弹查询
scripts/camera.js      平滑跟随玩家
```

- HUD 走 `emit` → `ctx.script_events` → kui：血量、金币、击杀数。
- **刻意不碰角色控制器**（`next.md` 里 P0 的那个空洞）：玩家与敌人都用纯变换
  移动 + 距离判定，不依赖 rapier 的 `KinematicCharacterController`。
  demo 因此不会撞在已知缺口上。
- `Cargo.toml` 加 `[[example]] name = "demo"`。

### 步骤 5 —— 收尾

- kscript 补测试：输入桥、spawn（含无效原型名、上限）、`node.script`
  （含节点已死时返回 null）。
- `cargo fmt` + `cargo clippy` + `cargo test -p kscript`。
- `cargo run --example demo` 实跑确认。

## 四、影响面

| crate | 改动 |
|---|---|
| kscript | host / bridge / prelude / runtime，+ 依赖 kinput |
| kapp | 传 `&mut Input` 给脚本；`App::with_prototype` |
| kscene | **不动** |
| 其它 | 不动 |

`kscene` 一行不改是有意的：它不认识脚本引擎，这条分层不能因为加功能就破掉。

## 五、执行结果

五步全部做完，另外**修掉了两个路上撞见的既有 bug**（都不是这次改动引入的，
但都挡着例子跑起来）：

### 已完成

| | 东西 | 位置 |
|---|---|---|
| 1 | 输入进桥（action / axis / 鼠标） | `kscript/{host,bridge,runtime}.rs` + `prelude.js` |
| 2 | `spawn` + 原型注册表（每 tick 上限 1024） | 同上 + `App::with_prototype` |
| 3 | `node.script` 脚本间调用（纯 JS 侧登记表） | `runtime.rs` + `prelude.js` |
| 4 | `node.forward`（`lookAt` 的读侧，子弹靠它取方向） | `bridge.rs` + `prelude.js` |
| 5 | demo：7 个脚本 + 200 行 Rust | `examples/kengine/demo/` |
| 6 | 12 条单元测试 + 7 条 demo 集成测试 | `kscript/object_tests.rs`、`tests/demo_scripts.rs` |

### 路上修掉的两个既有 bug

**一、`App` 内联了整个 `Runtime`，链式构建把主线程栈啃光。**

`App` 是 `.with_xxx(mut self) -> Self` 一路链下来的，而它内联着 `Runtime`
（`Scene` 11.5 KB + `ScriptRuntime` 24 KB + 渲染器…），debug 构建下每一环都在栈上
复制一整份。Windows 主线程只有 1 MB，链上七八个 `with_` 就到顶了——
症状是 `STATUS_STACK_OVERFLOW`，发生在 `resumed` 之前，一行日志都来不及打。
`script_hotreload` 已经中招（在这次改动之前就跑不起来）。

改成 `Option<Box<Runtime>>`，并加了一条 `size_of::<App>() < 1024` 的回归线。

**二、`kapp` 从不注册 `ScriptLoader`，所有脚本都是死的。**

`Node::with_script` 存的是资源路径，运行时每帧拿它去 `request::<Script>`——
但没人往 `ResourceManager` 里装脚本加载器，日志里只有一句
「没有能处理 `js` 的加载器」。挂了脚本的节点于是安安静静什么都不做。

现在 `kapp` 建资源管理器时自动装上（glTF、贴图那些仍由游戏自己注册：
那是游戏的资源，脚本加载器是引擎自己要用的）。

### 追加：park 每帧连造带拆一整个物理世界

问「为什么每帧都要去 request 脚本」时顺出来的。答案是**并没有**——
已实例化的槽位在三个布尔判断之后就跳过了，每帧真正发生的只是扫一遍
`script_nodes()`，实测 **6~8 ns/节点**（两千个节点约 13 µs，淹在噪声里）。

但量的过程中撞见了真正在烧钱的地方：`HostGuard::park` 里那句

```rust
let real_scene = std::mem::take(spare);   // take = 换上一个 Scene::default()
```

`mem::take` 会**新建一整个 `Scene`（含 rapier 物理世界）**顶替，`Drop` 里换出来
的空壳随后被析构。实测 `Scene::new + drop = 63.7 µs`，而 `kapp` 每帧至少 park
两次（`process` + 物理子步的 `physics_process`）。

这和那段代码自己的注释直接矛盾——注释说「空壳由运行时长期持有并复用」，
`take` 把这个意图破坏了。

改成 `spare: Option<Scene>`，park 时取出空壳顶上、`Drop` 时收回，
稳态下零分配零析构：

| | 每帧固定开销 |
|---|---:|
| 改之前 | **113 µs** |
| 改之后 | **3.2 µs** |

回归保护用的是记号而不是计时（计时断言在 CI 上必闪）：给空壳加一个节点，
被重造的话记号就没了 —— `host::test::the_empty_shell_is_recycled_not_rebuilt`。

### 追加：`prelude.js` 的内部字段改用私有字段

`self.position.y += dt` 这一行，每写一次要新建一个 `Node` 加一个
`BoundVector3`，而两个类的构造函数原本都在调 `Object.defineProperty`
（为了让 `_id` 不可枚举）。改成 ES 私有字段 `#id`：同样对外不可见
（而且更彻底），但不必每次都走一遍属性描述符流程。

**这一项没有可信的性能数字**——量它的时候机器已经进了热节流，
`empty_callback` 的置信区间跨了 5 倍。留在 `benches/script.rs` 里的
`raw_bridge` 那一档是为它准备的标尺：它绕开整个包装层直接捅桥，
和 `running` 的差值就是包装的价钱，且不受机器状态影响。
**在安静的机器上跑一次才能定论。**

已知的是数量级：`raw_bridge` 那一档测得很稳（334–410 µs / 100 实例），
而 `running` 最乐观也要 4.5 ms —— 包装层占了这条路径九成以上。
真要提脚本吞吐，那里才是下一个靶子。

### 验证

- `cargo test --workspace`：全绿（新增 19 条）。
- `cargo clippy --all-targets`：本次改动零警告
  （`kui_widgets` 里还有 4 条，是工作区里原本就有的）。
- `cargo run --example demo` 实跑：刷到第 13 波、7 次击杀、3 枚金币、
  血量掉到 61，脚本零异常。

