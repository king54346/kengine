//! JavaScript 运行时。
//!
//! **全 crate 唯一认识 boa 的地方**，与 `krender` 之于 wgpu、`kphysics` 之于
//! rapier、`kaudio` 之于 cpal 同一个道理。
//!
//! # 生命周期（照 GDScript）
//!
//! ```js
//! let speed = 2.0;
//!
//! return {
//!     _ready() { print("我醒了：", self.name); },
//!     _process(delta) { self.position.y += speed * delta; },
//!     _physics_process(delta) { self.applyImpulse(Vector3.UP().mul(delta)); },
//! };
//! ```
//!
//! - `_ready` 首次执行前一次；
//! - `_process(delta)` 每渲染帧一次；
//! - `_physics_process(delta)` 每物理子步一次，`delta` 恒等于物理步长。
//!
//! 写成函数体（而不是对象字面量）是为了让脚本有自己的**闭包变量**：
//! 上面的 `speed` 每实例一份，而不是所有实例共享一个全局。
//! 源码只解析一次——包成工厂函数，每实例化一次调一次工厂。

use crate::{
    bridge,
    host::{Host, HostGuard, with_host, with_scene},
    script::Script,
};
use boa_engine::{Context, JsObject, JsResult, JsValue, Source, js_string};
use kasset::ResourceManager;
use kcore::pool::Handle;
use kscene::{Node, Scene, ScriptSlot};
/// 脚本抛给游戏侧的一个信号。
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// 信号名。
    pub name: String,
    /// 附带的数值。
    pub value: f64,
    /// 发出它的节点。**在发出的那一刻**记下，不是事后猜的。
    pub source: Handle<Node>,
}

/// 一个脚本实例的编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u32);

/// 运行中的一个脚本实例。
struct Instance {
    object: JsObject,
    node: Handle<Node>,
    ready: bool,
    /// 出过错就停掉，不再调用任何方法。
    failed: bool,
    name: String,
}

/// 一次 tick 的统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptStats {
    /// 跑了多少个实例。
    pub ran: usize,
    /// 产生了多少个信号。
    pub signals: usize,
    /// 新出错、被停掉的脚本数。
    pub failed: usize,
}

/// JavaScript 运行时。
pub struct ScriptRuntime {
    context: Context,
    instances: Vec<Option<Instance>>,
    /// 宿主状态：节点登记处等，随场景一起进出线程局部。
    ///
    /// 放在运行时里而不是线程局部里长住：同一条线程上的两个运行时
    /// 否则会互相看到对方的句柄。
    host: Host,
    /// 寄存场景时用的空壳。
    ///
    /// 长期持有并复用：每次现造一个 `Scene` 要新建一整个物理世界（实测约 50 µs），
    /// 复用之后每帧只剩两次 6 KB 的 memcpy。
    spare: Scene,
    stats: ScriptStats,
}

impl std::fmt::Debug for ScriptRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptRuntime")
            .field("instances", &self.instance_count())
            .field("stats", &self.stats)
            .finish()
    }
}

impl Default for ScriptRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptRuntime {
    /// 循环迭代上限。
    ///
    /// 一帧里跑到一千万次循环的脚本一定是写错了。有这道闸，`while(true){}`
    /// 会抛异常然后被停掉，而不是把整个游戏挂死。
    pub const DEFAULT_LOOP_LIMIT: u64 = 10_000_000;

    /// 建一个装好引擎 API 的运行时。
    pub fn new() -> Self {
        let mut context = Context::default();

        let mut limits = context.runtime_limits();
        limits.set_loop_iteration_limit(Self::DEFAULT_LOOP_LIMIT);
        limits.set_recursion_limit(256);
        context.set_runtime_limits(limits);

        bridge::register(&mut context);

        // 前奏把扁平的桥包成 Node / Vector3 那套手感。
        if let Err(error) = context.eval(Source::from_bytes(include_str!("prelude.js"))) {
            // 前奏是编译期就固定的代码，跑不起来说明引擎自己坏了，
            // 早失败早暴露，比让每个脚本都莫名其妙地报错强。
            panic!("kscript 前奏脚本执行失败：{error}");
        }

        Self {
            context,
            instances: Vec::new(),
            host: Host::default(),
            spare: Scene::new(),
            stats: ScriptStats::default(),
        }
    }

    /// 注册一个模块，脚本里用 `require("名字")` 取。
    ///
    /// ```js
    /// // 模块 "math_utils"：
    /// exports.lerp = (a, b, t) => a + (b - a) * t;
    ///
    /// // 脚本里：
    /// const utils = require("math_utils");
    /// return { _process(dt) { self.position.y = utils.lerp(0, 5, dt); } };
    /// ```
    ///
    /// # 模块是懒执行的
    ///
    /// 这里只存源码，第一次 `require` 时才真正跑一遍，之后走缓存。
    /// 所以注册一个语法有错的模块**不会立刻报错**——没人 require 它就一直
    /// 相安无事。
    ///
    /// # 重新注册会清掉缓存
    ///
    /// 热重载时用同一个名字再注册一次，下次 `require` 会拿到新版本。
    /// 但**已经 require 过它的脚本手里还攥着旧的 exports**——那是它们
    /// 实例化时就抓在闭包里的引用，这里够不着。要彻底换掉得连脚本一起重载。
    ///
    /// 返回 `false` 表示运行时内部状态异常（正常情况下不会发生）。
    pub fn add_module(&mut self, name: &str, source: &str) -> bool {
        // 直接操作对象属性，不是拼一段 JS 去 eval——源码里带引号或反斜杠
        // 的话，拼字符串会拼出语法错误，甚至让模块源码逃逸成可执行代码。
        let Ok(sources) = self
            .context
            .global_object()
            .get(js_string!("__moduleSources"), &mut self.context)
        else {
            klog::error!("找不到 __moduleSources，前奏脚本没跑成功？");
            return false;
        };
        let Some(sources) = sources.as_object() else {
            klog::error!("__moduleSources 不是对象");
            return false;
        };

        if sources
            .set(
                js_string!(name),
                js_string!(source),
                true,
                &mut self.context,
            )
            .is_err()
        {
            klog::error!("模块「{name}」写入失败");
            return false;
        }

        self.invalidate_module(name);
        true
    }

    /// 一个模块是否已注册。
    pub fn has_module(&mut self, name: &str) -> bool {
        let Ok(sources) = self
            .context
            .global_object()
            .get(js_string!("__moduleSources"), &mut self.context)
        else {
            return false;
        };
        sources
            .as_object()
            .and_then(|o| o.get(js_string!(name), &mut self.context).ok())
            .is_some_and(|v| v.is_string())
    }

    /// 把一个模块从缓存里剔除，下次 `require` 会重新执行它。
    fn invalidate_module(&mut self, name: &str) {
        let Ok(cache) = self
            .context
            .global_object()
            .get(js_string!("__moduleCache"), &mut self.context)
        else {
            return;
        };
        if let Some(cache) = cache.as_object() {
            let key: boa_engine::property::PropertyKey = js_string!(name).into();
            let _ = cache.delete_property_or_throw(key, &mut self.context);
        }
    }

    /// 上一次 tick 的统计。
    pub fn stats(&self) -> ScriptStats {
        self.stats
    }

    /// 活着的实例数。
    pub fn instance_count(&self) -> usize {
        self.instances.iter().filter(|slot| slot.is_some()).count()
    }

    /// 某个实例是否已经因为出错被停掉。
    pub fn is_failed(&self, id: InstanceId) -> bool {
        self.instances
            .get(id.0 as usize)
            .and_then(Option::as_ref)
            .is_none_or(|instance| instance.failed)
    }

    /// 每渲染帧调一次：实例化新脚本，调 `_ready` 与 `_process`。
    pub fn process(
        &mut self,
        scene: &mut Scene,
        resources: &ResourceManager,
        dt: f32,
        elapsed: f32,
    ) -> Vec<Signal> {
        self.stats = ScriptStats::default();
        self.instantiate_pending(scene, resources);
        self.run("_process", scene, dt, elapsed)
    }

    /// 每物理子步调一次：调 `_physics_process`。
    ///
    /// `dt` 恒等于物理步长——在这里写 `v * delta` 却拿到帧间隔的话，
    /// 定长调度就白做了，那正是它要消除的东西。
    pub fn physics_process(&mut self, scene: &mut Scene, dt: f32, elapsed: f32) -> Vec<Signal> {
        self.run("_physics_process", scene, dt, elapsed)
    }

    /// 把场景里还没实例化的脚本槽位建出来。
    fn instantiate_pending(&mut self, scene: &mut Scene, resources: &ResourceManager) {
        for position in 0..scene.script_nodes().len() {
            let handle = scene.script_nodes()[position];
            let Some(slot) = scene.try_get(handle).and_then(Node::script) else {
                continue;
            };
            if slot.failed || slot.is_live() || !slot.enabled {
                continue;
            }

            let path = slot.path.clone();
            // 脚本是异步加载的，没就绪就等下一帧。
            let Some(script) = resources
                .request::<Script>(&path)
                .data_ref()
                .map(|data| data.clone())
            else {
                continue;
            };

            let instance = self.instantiate(&script, handle);
            if let Some(slot) = scene.try_get_mut(handle).and_then(Node::script_mut) {
                match instance {
                    Some(id) => slot.instance = id.0,
                    None => slot.failed = true,
                }
            }
        }
    }

    /// 实例化一份脚本。
    ///
    /// 源码有语法错误、或者返回的不是对象时返回 [`None`] 并记一条日志——
    /// 一个坏脚本不该让整个场景起不来。
    pub fn instantiate(&mut self, script: &Script, node: Handle<Node>) -> Option<InstanceId> {
        let factory = match self.context.eval(Source::from_bytes(&script.as_factory())) {
            Ok(value) => value,
            Err(error) => {
                klog::error!("脚本「{}」解析失败：{error}", script.name());
                return None;
            }
        };

        let Some(callable) = factory.as_callable() else {
            klog::error!("脚本「{}」没有产出可调用的工厂函数", script.name());
            return None;
        };

        let produced = match callable.call(&JsValue::undefined(), &[], &mut self.context) {
            Ok(value) => value,
            Err(error) => {
                klog::error!("脚本「{}」初始化时抛异常：{error}", script.name());
                return None;
            }
        };

        let Some(object) = produced.as_object() else {
            klog::error!(
                "脚本「{}」必须 `return` 一个对象（可带 _ready / _process / _physics_process）",
                script.name()
            );
            return None;
        };

        let instance = Instance {
            object: object.clone(),
            node,
            ready: false,
            failed: false,
            name: script.name().to_string(),
        };

        // 优先填回收掉的槽位，实例数才不会随反复增删无限涨。
        let id = match self.instances.iter().position(Option::is_none) {
            Some(index) => {
                self.instances[index] = Some(instance);
                index
            }
            None => {
                self.instances.push(Some(instance));
                self.instances.len() - 1
            }
        };
        Some(InstanceId(id as u32))
    }

    /// 跑一轮生命周期回调。
    fn run(&mut self, method: &str, scene: &mut Scene, dt: f32, elapsed: f32) -> Vec<Signal> {
        // 场景与宿主状态整个搬进线程局部，原生函数于是能拿到真正的 `&mut Scene`。
        let guard = HostGuard::park(scene, &mut self.host, &mut self.spare, dt, elapsed);

        for index in 0..self.instances.len() {
            let Some(instance) = self.instances[index].as_ref() else {
                continue;
            };
            if instance.failed {
                continue;
            }

            let (object, node, ready, name) = (
                instance.object.clone(),
                instance.node,
                instance.ready,
                instance.name.clone(),
            );

            // 节点没了就把实例一起收掉——留着它每帧对着失效句柄空转。
            let alive = with_scene(|scene| scene.try_get(node).is_some()).unwrap_or(false);
            if !alive {
                self.instances[index] = None;
                continue;
            }

            with_host(|host| host.current = node);

            if !ready {
                if let Err(error) = call_method(&mut self.context, &object, "_ready", &[]) {
                    fail(
                        &mut self.instances,
                        &mut self.stats,
                        index,
                        &name,
                        "_ready",
                        &error,
                    );
                    continue;
                }
                if let Some(instance) = self.instances[index].as_mut() {
                    instance.ready = true;
                }
            }

            let args = [JsValue::from(dt as f64)];
            if let Err(error) = call_method(&mut self.context, &object, method, &args) {
                fail(
                    &mut self.instances,
                    &mut self.stats,
                    index,
                    &name,
                    method,
                    &error,
                );
                continue;
            }
            self.stats.ran += 1;
        }

        drop(guard);

        // guard 落下时宿主状态（含这一轮攒的信号）已经还回 `self.host`。
        let signals = std::mem::take(&mut self.host.signals);
        self.stats.signals = signals.len();
        signals
    }

    /// 某个脚本文件重新加载了：把用它的实例全部作废，下一帧重建。
    ///
    /// 返回作废了几个。资源本身由 `kasset` 的热重载负责换掉，这里只管
    /// 「运行时手里那个旧实例得扔掉」——不扔的话文件改了也没反应，
    /// 看起来像热重载坏了。
    ///
    /// **脚本内部的状态会丢**：新实例从头开始，闭包变量回到初值。
    /// 要保住得让脚本自己实现存取接口，那是另一件事（见 PLAN 的未做项）。
    pub fn reload_path(&mut self, scene: &mut Scene, path: &std::path::Path) -> usize {
        let wanted = normalize(&path.to_string_lossy());
        let mut reset = 0;

        for position in 0..scene.script_nodes().len() {
            let handle = scene.script_nodes()[position];
            let Some(slot) = scene.try_get_mut(handle).and_then(Node::script_mut) else {
                continue;
            };
            if normalize(&slot.path) != wanted {
                continue;
            }

            if slot.is_live() {
                // 槽位记的下标就是实例数组的下标。
                if let Some(entry) = self.instances.get_mut(slot.instance as usize) {
                    *entry = None;
                }
            }
            slot.instance = ScriptSlot::NO_INSTANCE;
            // 上次因为语法错误停用的，改好之后该重新给一次机会。
            slot.failed = false;
            reset += 1;
        }

        reset
    }

    /// 销毁全部实例并清空句柄登记处。切场景时调。
    pub fn clear(&mut self) {
        self.instances.clear();
        self.host = Host::default();
    }
}

/// 停掉一个出错的实例。
///
/// 停掉而不是每帧重试：抛异常的脚本几乎必然每帧都抛，不停的话日志会被刷爆，
/// 真正的第一条错误反而找不到了。
///
/// 写成自由函数而不是方法：跑脚本期间 `self.host` 正被寄存凭证借着，
/// 一个吃掉整个 `&mut self` 的方法在这里过不了借用检查。
fn fail(
    instances: &mut [Option<Instance>],
    stats: &mut ScriptStats,
    index: usize,
    name: &str,
    method: &str,
    error: &boa_engine::JsError,
) {
    klog::error!("脚本「{name}」的 {method} 抛异常，已停用该脚本：{error}");
    if let Some(instance) = instances[index].as_mut() {
        instance.failed = true;
    }
    stats.failed += 1;
}

/// 调用实例上的一个方法。方法不存在时安静跳过——生命周期方法都是可选的。
fn call_method(
    context: &mut Context,
    object: &JsObject,
    method: &str,
    args: &[JsValue],
) -> JsResult<JsValue> {
    let function = object.get(js_string!(method), context)?;
    let Some(callable) = function.as_callable() else {
        return Ok(JsValue::undefined());
    };
    callable.call(&object.clone().into(), args, context)
}

/// 把路径统一成比较用的形式。
///
/// Windows 上写 `assets\a.js`、别处写 `assets/a.js` 指的是同一个文件，
/// 不统一的话热重载在其中一个平台上就是不响。
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}
