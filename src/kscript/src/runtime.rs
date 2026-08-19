//! JavaScript 运行时：boa 引擎的封装。
//!
//! **全 crate 唯一认识 boa 的地方**，作用与 `krender` 之于 wgpu、
//! `kphysics` 之于 rapier、`kaudio` 之于 cpal 相同。
//!
//! # 宿主状态怎么递进 VM
//!
//! boa 的原生函数必须是 `'static`，闭包捕获还要么 `Copy`、要么走 `unsafe`。
//! 这里用**作用域内的线程局部 [`RefCell`]**：一次 tick 开始时把快照与命令缓冲
//! 放进去，结束时（靠 `Drop`，panic 也照样执行）取回来。全程安全 Rust，
//! 没有裸指针，没有 `unsafe`。
//!
//! 线程局部在这里是恰当的：boa 的 `Context` 本来就不是 `Send`，
//! 一个运行时只能在一条线程上跑，跨线程共享状态的问题根本不存在。
//!
//! # 跑飞的脚本
//!
//! 三道闸：boa 自带的**循环迭代上限**与**递归上限**（死循环会抛异常而不是挂死）、
//! [`CommandBuffer`] 的**命令条数上限**（狂发命令不会撑爆内存）、
//! 以及**出错即禁用**（抛异常的脚本停掉并记一条日志，而不是每帧刷屏）。

use crate::{
    api::{Command, CommandBuffer, NodeRef, Snapshot},
    script::Script,
};
use boa_engine::{
    Context, JsObject, JsResult, JsValue, NativeFunction, Source, js_string,
    object::ObjectInitializer, property::Attribute,
};
use kmath::{Quat, Vec3};
use std::cell::RefCell;

thread_local! {
    /// 当前 tick 的宿主状态。只在 [`ScriptRuntime::tick`] 期间非空。
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// tick 期间原生函数能碰到的东西。
struct Host {
    snapshot: Snapshot,
    commands: CommandBuffer,
    /// 当前正在跑的脚本挂在哪个节点上，`engine.self()` 读它。
    current: NodeRef,
    /// 因为含 NaN / 无穷而被丢掉的命令数。
    rejected: usize,
}

/// 进入 tick 时装上宿主状态，离开时取回。
///
/// 用 `Drop` 而不是手动收尾：脚本抛异常时中间那段代码会被跳过，
/// 宿主状态就永远留在那儿了——下一帧再进来会读到上一帧的快照。
struct HostGuard;

impl Drop for HostGuard {
    fn drop(&mut self) {
        HOST.with(|host| host.borrow_mut().take());
    }
}

/// 借用宿主状态。不在 tick 期间时返回 `None`。
fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    HOST.with(|host| host.borrow_mut().as_mut().map(f))
}

/// 从参数里取一个 `f64`，缺参数当 0。
fn number(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<f64> {
    match args.get(index) {
        Some(value) => value.to_number(context),
        None => Ok(0.0),
    }
}

/// 从参数里取一个节点编号。
fn node_ref(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<NodeRef> {
    Ok(NodeRef::from_js(number(args, index, context)?))
}

/// 从参数里取三个数当向量。
fn vec3(args: &[JsValue], start: usize, context: &mut Context) -> JsResult<Vec3> {
    Ok(Vec3::new(
        number(args, start, context)? as f32,
        number(args, start + 1, context)? as f32,
        number(args, start + 2, context)? as f32,
    ))
}

/// 往缓冲里塞一条命令，非有限的数值会被拦下。
///
/// NaN 是这类接口最阴的一种输入：一个 `0/0` 写进位置，世界矩阵会变成 NaN，
/// 包围盒随之变成 NaN，剔除把它判成不可见——**整个物体无声无息地消失**，
/// 而日志里什么都没有。在边界上拦掉，比事后追查便宜得多。
fn submit(command: Command) {
    let finite = match &command {
        Command::SetPosition(_, v)
        | Command::Translate(_, v)
        | Command::SetScale(_, v)
        | Command::ApplyImpulse(_, v) => v.is_finite(),
        Command::SetRotation(_, q) => q.is_finite(),
        Command::RotateY(_, angle) => angle.is_finite(),
        Command::Emit { value, .. } => value.is_finite(),
        _ => true,
    };

    with_host(|host| {
        if finite {
            host.commands.push(command);
        } else {
            host.rejected += 1;
        }
    });
}

/// 建一个 `{x, y, z}` 对象还给 JS。
fn vec3_to_js(value: Vec3, context: &mut Context) -> JsValue {
    ObjectInitializer::new(context)
        .property(js_string!("x"), value.x as f64, Attribute::all())
        .property(js_string!("y"), value.y as f64, Attribute::all())
        .property(js_string!("z"), value.z as f64, Attribute::all())
        .build()
        .into()
}

/// 一个脚本实例的编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(u32);

/// 运行中的一个脚本实例。
struct Instance {
    object: JsObject,
    /// 挂在哪个节点上。每帧由调用方刷新——节点在快照里的编号会变。
    node: NodeRef,
    /// `init` 有没有调过。
    initialized: bool,
    /// 出过错就停掉，不再调用任何方法。
    failed: bool,
    /// 脚本名，报错时用。
    name: String,
}

/// 一次 tick 的统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptStats {
    /// 这一帧跑了多少个实例。
    pub ran: usize,
    /// 这一帧产生了多少条命令。
    pub commands: usize,
    /// 因为超出上限被丢掉的命令数。
    pub dropped: usize,
    /// 因为含 NaN 被拦下的命令数。
    pub rejected: usize,
    /// 这一帧新出错、被停掉的脚本数。
    pub failed: usize,
}

/// JavaScript 运行时。
pub struct ScriptRuntime {
    context: Context,
    instances: Vec<Option<Instance>>,
    /// 一帧最多接受多少条命令。
    command_limit: usize,
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
    /// 默认的循环迭代上限。
    ///
    /// 一帧里跑到一千万次循环的脚本一定是写错了。有这道闸，
    /// `while(true){}` 会抛异常然后被停掉，而不是把整个游戏挂死。
    pub const DEFAULT_LOOP_LIMIT: u64 = 10_000_000;

    /// 建一个装好引擎 API 的运行时。
    pub fn new() -> Self {
        let mut context = Context::default();

        let mut limits = context.runtime_limits();
        limits.set_loop_iteration_limit(Self::DEFAULT_LOOP_LIMIT);
        limits.set_recursion_limit(256);
        context.set_runtime_limits(limits);

        register_engine_api(&mut context);

        Self {
            context,
            instances: Vec::new(),
            command_limit: CommandBuffer::DEFAULT_LIMIT,
            stats: ScriptStats::default(),
        }
    }

    /// 指定每帧的命令上限。
    pub fn with_command_limit(mut self, limit: usize) -> Self {
        self.command_limit = limit.max(1);
        self
    }

    /// 上一帧的统计。
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

    /// 实例化一个脚本，返回它的编号。
    ///
    /// 源码有语法错误、或者返回的不是对象时返回 [`None`] 并记一条日志——
    /// 一个坏脚本不该让整个场景加载失败。
    pub fn instantiate(&mut self, script: &Script, node: NodeRef) -> Option<InstanceId> {
        let factory = match self
            .context
            .eval(Source::from_bytes(&script.as_factory()))
        {
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
                "脚本「{}」必须 `return` 一个对象（可带 init / update / destroy）",
                script.name()
            );
            return None;
        };

        let instance = Instance {
            object: object.clone(),
            node,
            initialized: false,
            failed: false,
            name: script.name().to_string(),
        };

        // 优先填回收掉的槽位，实例数才不会随着反复增删无限涨。
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

    /// 销毁一个实例，销毁前调它的 `destroy`。
    pub fn destroy(&mut self, id: InstanceId, snapshot: Snapshot) {
        let Some(slot) = self.instances.get_mut(id.0 as usize) else {
            return;
        };
        let Some(instance) = slot.take() else {
            return;
        };

        if instance.failed || !instance.initialized {
            return;
        }

        // `destroy` 里也可能想发命令（比如放一个消失音效），
        // 所以照样要把宿主状态装上。
        let _guard = self.enter(snapshot);
        let node = instance.node;
        let _ = call_method(&mut self.context, &instance.object, "destroy", &[], node);
    }

    /// 把某个实例绑定的节点编号刷新一遍。
    ///
    /// 节点在快照里的编号每帧都可能变（有节点增删时），
    /// 调用方在建快照时顺手把新编号告诉运行时。
    pub fn rebind(&mut self, id: InstanceId, node: NodeRef) {
        if let Some(Some(instance)) = self.instances.get_mut(id.0 as usize) {
            instance.node = node;
        }
    }

    /// 跑一帧：对每个实例调 `init`（首次）与 `update(dt)`，收集命令。
    ///
    /// `snapshot` 是本帧的场景快照；返回脚本发出的命令，由调用方落地。
    pub fn tick(&mut self, snapshot: Snapshot) -> Vec<Command> {
        let dt = snapshot.dt;
        self.stats = ScriptStats::default();

        let guard = self.enter(snapshot);

        for index in 0..self.instances.len() {
            let Some(instance) = self.instances[index].as_ref() else {
                continue;
            };
            if instance.failed {
                continue;
            }

            let (object, node, initialized, name) = (
                instance.object.clone(),
                instance.node,
                instance.initialized,
                instance.name.clone(),
            );

            if !initialized {
                if let Err(error) = call_method(&mut self.context, &object, "init", &[], node) {
                    self.fail(index, &name, "init", &error);
                    continue;
                }
                if let Some(instance) = self.instances[index].as_mut() {
                    instance.initialized = true;
                }
            }

            let args = [JsValue::from(dt as f64)];
            if let Err(error) = call_method(&mut self.context, &object, "update", &args, node) {
                self.fail(index, &name, "update", &error);
                continue;
            }

            self.stats.ran += 1;
        }

        // 在 guard 落下之前把命令取出来。
        // 先读计数再 `take`：`take` 会把 `dropped` 清零，而 Rust 的元组是
        // 从左到右求值的——顺序反了计数就永远是 0。
        let (commands, dropped, rejected) = with_host(|host| {
            let dropped = host.commands.dropped();
            let rejected = host.rejected;
            (host.commands.take(), dropped, rejected)
        })
        .unwrap_or_default();
        drop(guard);

        self.stats.commands = commands.len();
        self.stats.dropped = dropped;
        self.stats.rejected = rejected;
        if dropped > 0 {
            klog::warn!("脚本这一帧发了太多命令，丢弃了 {dropped} 条");
        }
        if rejected > 0 {
            klog::warn!("脚本发出了 {rejected} 条含 NaN 的命令，已拦下");
        }

        commands
    }

    /// 装上宿主状态，返回一个到期自动卸下的守卫。
    fn enter(&mut self, snapshot: Snapshot) -> HostGuard {
        HOST.with(|host| {
            *host.borrow_mut() = Some(Host {
                snapshot,
                commands: CommandBuffer::with_limit(self.command_limit),
                current: NodeRef::NONE,
                rejected: 0,
            });
        });
        HostGuard
    }

    /// 把一个实例标记为出错并停掉。
    ///
    /// 停掉而不是每帧重试：一个抛异常的脚本几乎必然每帧都抛，
    /// 不停的话日志会被刷爆，真正的第一条错误反而找不到了。
    fn fail(&mut self, index: usize, name: &str, method: &str, error: &boa_engine::JsError) {
        klog::error!("脚本「{name}」的 {method} 抛异常，已停用该脚本：{error}");
        if let Some(instance) = self.instances[index].as_mut() {
            instance.failed = true;
        }
        self.stats.failed += 1;
    }
}

/// 调用实例上的一个方法。方法不存在时安静跳过——三个生命周期方法都是可选的。
fn call_method(
    context: &mut Context,
    object: &JsObject,
    method: &str,
    args: &[JsValue],
    node: NodeRef,
) -> JsResult<JsValue> {
    let key = js_string!(method);
    let function = object.get(key, context)?;
    let Some(callable) = function.as_callable() else {
        return Ok(JsValue::undefined());
    };

    // 告诉 `engine.self()` 当前是谁在跑。
    with_host(|host| host.current = node);

    callable.call(&object.clone().into(), args, context)
}

/// 往全局装上 `engine` 对象。
fn register_engine_api(context: &mut Context) {
    let engine = ObjectInitializer::new(context)
        // ── 读：全部走快照 ──
        .function(
            NativeFunction::from_fn_ptr(|_, _, _| {
                Ok(JsValue::from(
                    with_host(|host| host.current.to_js()).unwrap_or(NodeRef::NONE.to_js()),
                ))
            }),
            js_string!("self"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, _, _| {
                Ok(JsValue::from(
                    with_host(|host| host.snapshot.elapsed as f64).unwrap_or(0.0),
                ))
            }),
            js_string!("time"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = match args.first() {
                    Some(value) => value.to_string(context)?.to_std_string_escaped(),
                    None => String::new(),
                };
                Ok(JsValue::from(
                    with_host(|host| host.snapshot.find(&name).to_js())
                        .unwrap_or(NodeRef::NONE.to_js()),
                ))
            }),
            js_string!("find"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                let position = with_host(|host| {
                    host.snapshot.node(node).map(|state| state.position)
                })
                .flatten();
                Ok(match position {
                    Some(value) => vec3_to_js(value, context),
                    None => JsValue::null(),
                })
            }),
            js_string!("position"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                let position = with_host(|host| {
                    host.snapshot.node(node).map(|state| state.world_position)
                })
                .flatten();
                Ok(match position {
                    Some(value) => vec3_to_js(value, context),
                    None => JsValue::null(),
                })
            }),
            js_string!("worldPosition"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                let name = with_host(|host| {
                    host.snapshot.node(node).map(|state| state.name.clone())
                })
                .flatten();
                Ok(match name {
                    Some(name) => JsValue::from(js_string!(name.as_str())),
                    None => JsValue::null(),
                })
            }),
            js_string!("name"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                let visible =
                    with_host(|host| host.snapshot.node(node).map(|state| state.visible)).flatten();
                Ok(JsValue::from(visible.unwrap_or(false)))
            }),
            js_string!("visible"),
            1,
        )
        // ── 写：全部变成命令 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                submit(Command::SetPosition(node, vec3(args, 1, context)?));
                Ok(JsValue::undefined())
            }),
            js_string!("setPosition"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                submit(Command::Translate(node, vec3(args, 1, context)?));
                Ok(JsValue::undefined())
            }),
            js_string!("translate"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                let angle = number(args, 1, context)? as f32;
                submit(Command::RotateY(node, angle));
                Ok(JsValue::undefined())
            }),
            js_string!("rotateY"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                submit(Command::SetScale(node, vec3(args, 1, context)?));
                Ok(JsValue::undefined())
            }),
            js_string!("setScale"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                let visible = args.get(1).map(JsValue::to_boolean).unwrap_or(false);
                submit(Command::SetVisible(node, visible));
                Ok(JsValue::undefined())
            }),
            js_string!("setVisible"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                submit(Command::ApplyImpulse(node, vec3(args, 1, context)?));
                Ok(JsValue::undefined())
            }),
            js_string!("applyImpulse"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                submit(Command::PlaySound(node));
                Ok(JsValue::undefined())
            }),
            js_string!("playSound"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let node = node_ref(args, 0, context)?;
                submit(Command::Despawn(node));
                Ok(JsValue::undefined())
            }),
            js_string!("despawn"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let text = match args.first() {
                    Some(value) => value.to_string(context)?.to_std_string_escaped(),
                    None => String::new(),
                };
                submit(Command::Log(text));
                Ok(JsValue::undefined())
            }),
            js_string!("log"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = match args.first() {
                    Some(value) => value.to_string(context)?.to_std_string_escaped(),
                    None => String::new(),
                };
                let value = number(args, 1, context)?;
                let source = with_host(|host| host.current).unwrap_or(NodeRef::NONE);
                submit(Command::Emit {
                    name,
                    value,
                    source,
                });
                Ok(JsValue::undefined())
            }),
            js_string!("emit"),
            2,
        )
        .build();

    context
        .register_global_property(js_string!("engine"), engine, Attribute::all())
        .expect("engine 是刚建的运行时里第一个全局属性，不该冲突");
}

/// 让 [`Quat`] 也能做有限性检查。
trait Finite {
    fn is_finite(&self) -> bool;
}

impl Finite for Quat {
    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.w.is_finite()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::api::NodeState;

    /// 跑一个脚本一帧，返回它发出的命令。
    fn run(source: &str) -> Vec<Command> {
        run_with(source, Snapshot::new(0.016, 1.0))
    }

    /// 跑一个脚本一帧，快照自定。
    fn run_with(source: &str, snapshot: Snapshot) -> Vec<Command> {
        let mut runtime = ScriptRuntime::new();
        let script = Script::new(source, "test.js");
        runtime
            .instantiate(&script, NodeRef(0))
            .expect("脚本应当能实例化");
        runtime.tick(snapshot)
    }

    /// 一个含两个节点的快照。
    fn two_nodes() -> Snapshot {
        let mut snapshot = Snapshot::new(0.5, 2.0);
        snapshot.push(NodeState {
            name: "player".into(),
            position: Vec3::new(1.0, 2.0, 3.0),
            world_position: Vec3::new(10.0, 20.0, 30.0),
            visible: true,
            ..Default::default()
        });
        snapshot.push(NodeState {
            name: "enemy".into(),
            position: Vec3::new(-1.0, 0.0, 0.0),
            visible: false,
            ..Default::default()
        });
        snapshot
    }

    #[test]
    fn a_script_without_any_lifecycle_methods_is_harmless() {
        // 三个方法都是可选的，什么都不实现也不该出错。
        assert!(run("return {};").is_empty());
    }

    #[test]
    fn update_runs_and_receives_the_delta_time() {
        let commands = run("return { update(dt) { engine.log('dt=' + dt); } };");

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            Command::Log(text) => assert!(text.starts_with("dt=0.016"), "实际是 {text}"),
            other => panic!("发出来的是 {other:?}"),
        }
    }

    #[test]
    fn init_runs_once_before_the_first_update() {
        let mut runtime = ScriptRuntime::new();
        let script = Script::new(
            "return { init() { engine.log('init'); }, update() { engine.log('update'); } };",
            "t.js",
        );
        runtime.instantiate(&script, NodeRef(0)).unwrap();

        let first = runtime.tick(Snapshot::new(0.016, 0.0));
        let second = runtime.tick(Snapshot::new(0.016, 0.016));

        assert_eq!(
            first,
            vec![Command::Log("init".into()), Command::Log("update".into())]
        );
        assert_eq!(second, vec![Command::Log("update".into())], "init 跑了不止一次");
    }

    #[test]
    fn each_instance_gets_its_own_closure_state() {
        // 写成函数体（而不是对象字面量）就是为了这个：`n` 每实例一份。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new(
            "let n = 0; return { update() { n += 1; engine.emit('n', n); } };",
            "t.js",
        );
        runtime.instantiate(&script, NodeRef(0)).unwrap();
        runtime.instantiate(&script, NodeRef(1)).unwrap();

        runtime.tick(Snapshot::new(0.016, 0.0));
        let commands = runtime.tick(Snapshot::new(0.016, 0.016));

        // 两个实例各自数到 2，而不是一个数到 4；来源也各是各的。
        assert_eq!(
            commands,
            vec![
                Command::Emit {
                    name: "n".into(),
                    value: 2.0,
                    source: NodeRef(0),
                },
                Command::Emit {
                    name: "n".into(),
                    value: 2.0,
                    source: NodeRef(1),
                }
            ]
        );
    }

    #[test]
    fn destroy_runs_when_the_instance_goes_away() {
        let mut runtime = ScriptRuntime::new();
        let script = Script::new(
            "return { update() {}, destroy() { engine.log('bye'); } };",
            "t.js",
        );
        let id = runtime.instantiate(&script, NodeRef(0)).unwrap();
        runtime.tick(Snapshot::new(0.016, 0.0));

        runtime.destroy(id, Snapshot::new(0.0, 1.0));

        assert_eq!(runtime.instance_count(), 0);
    }

    #[test]
    fn destroy_is_skipped_for_a_script_that_never_started() {
        // 没跑过 `init` 的实例不该收到 `destroy`——它根本没「活」过。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { destroy() { engine.log('bye'); } };", "t.js");
        let id = runtime.instantiate(&script, NodeRef(0)).unwrap();

        runtime.destroy(id, Snapshot::new(0.0, 0.0));

        assert_eq!(runtime.instance_count(), 0);
    }

    // ── 读接口 ──

    #[test]
    fn a_script_can_read_its_own_node() {
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { update() { engine.log(engine.name(engine.self())); } };", "t.js");
        runtime.instantiate(&script, NodeRef(1)).unwrap();

        let commands = runtime.tick(two_nodes());

        assert_eq!(commands, vec![Command::Log("enemy".into())]);
    }

    #[test]
    fn nodes_can_be_looked_up_by_name() {
        let commands = run_with(
            "return { update() { engine.log('found ' + engine.find('enemy')); } };",
            two_nodes(),
        );

        assert_eq!(commands, vec![Command::Log("found 1".into())]);
    }

    #[test]
    fn looking_up_a_missing_name_gives_an_unusable_ref() {
        let commands = run_with(
            "return { update() { engine.log('' + (engine.find('nobody') === 4294967295)); } };",
            two_nodes(),
        );

        assert_eq!(commands, vec![Command::Log("true".into())]);
    }

    #[test]
    fn positions_come_through_as_readable_objects() {
        let commands = run_with(
            "return { update() { let p = engine.position(0); engine.log(p.x + ',' + p.y + ',' + p.z); } };",
            two_nodes(),
        );

        assert_eq!(commands, vec![Command::Log("1,2,3".into())]);
    }

    #[test]
    fn world_and_local_positions_are_distinct() {
        let commands = run_with(
            "return { update() { engine.log('' + engine.worldPosition(0).x); } };",
            two_nodes(),
        );

        assert_eq!(commands, vec![Command::Log("10".into())]);
    }

    #[test]
    fn reading_a_nonexistent_node_gives_null_rather_than_throwing() {
        // 脚本拿到一个过期编号是常态（节点这一帧被删了），不该整个崩掉。
        let commands = run_with(
            "return { update() { engine.log('' + (engine.position(99) === null)); } };",
            two_nodes(),
        );

        assert_eq!(commands, vec![Command::Log("true".into())]);
    }

    // ── 写接口 ──

    #[test]
    fn every_write_becomes_a_command() {
        let commands = run(
            "return { update() {
                engine.setPosition(0, 1, 2, 3);
                engine.translate(0, 0, 1, 0);
                engine.rotateY(0, 0.5);
                engine.setScale(0, 2, 2, 2);
                engine.setVisible(0, false);
                engine.applyImpulse(0, 0, 5, 0);
                engine.playSound(0);
                engine.despawn(0);
            } };",
        );

        assert_eq!(
            commands,
            vec![
                Command::SetPosition(NodeRef(0), Vec3::new(1.0, 2.0, 3.0)),
                Command::Translate(NodeRef(0), Vec3::Y),
                Command::RotateY(NodeRef(0), 0.5),
                Command::SetScale(NodeRef(0), Vec3::splat(2.0)),
                Command::SetVisible(NodeRef(0), false),
                Command::ApplyImpulse(NodeRef(0), Vec3::new(0.0, 5.0, 0.0)),
                Command::PlaySound(NodeRef(0)),
                Command::Despawn(NodeRef(0)),
            ]
        );
    }

    #[test]
    fn missing_arguments_default_to_zero_instead_of_throwing() {
        let commands = run("return { update() { engine.setPosition(0); } };");

        assert_eq!(commands, vec![Command::SetPosition(NodeRef(0), Vec3::ZERO)]);
    }

    #[test]
    fn writes_do_not_show_up_in_the_same_frames_snapshot() {
        // 快照进、命令出：这一帧写的东西下一帧才看得到。
        // 语义要么这样、要么让脚本互相看到对方的中间状态——后者更难推理。
        let commands = run_with(
            "return { update() {
                engine.setPosition(0, 99, 99, 99);
                engine.log('' + engine.position(0).x);
            } };",
            two_nodes(),
        );

        assert_eq!(
            commands,
            vec![
                Command::SetPosition(NodeRef(0), Vec3::splat(99.0)),
                Command::Log("1".into()),
            ]
        );
    }

    // ── 抗打击 ──

    #[test]
    fn a_nan_never_reaches_the_scene() {
        // NaN 写进位置，世界矩阵会变 NaN，包围盒变 NaN，剔除判它不可见——
        // 整个物体无声无息地消失，日志里什么都没有。必须在边界拦掉。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new(
            "return { update() { engine.setPosition(0, 0/0, 1, 2); engine.rotateY(0, 1/0); } };",
            "t.js",
        );
        runtime.instantiate(&script, NodeRef(0)).unwrap();

        let commands = runtime.tick(Snapshot::new(0.016, 0.0));

        assert!(commands.is_empty(), "含 NaN 的命令漏进来了：{commands:?}");
        assert_eq!(runtime.stats().rejected, 2);
    }

    #[test]
    fn a_syntax_error_fails_instantiation_without_panicking() {
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { this is not javascript };", "bad.js");

        assert!(runtime.instantiate(&script, NodeRef(0)).is_none());
        assert_eq!(runtime.instance_count(), 0);
    }

    #[test]
    fn a_script_that_returns_a_non_object_is_rejected() {
        let mut runtime = ScriptRuntime::new();

        assert!(
            runtime
                .instantiate(&Script::new("return 42;", "n.js"), NodeRef(0))
                .is_none()
        );
    }

    #[test]
    fn a_throwing_script_is_disabled_instead_of_spamming_every_frame() {
        // 抛异常的脚本几乎必然每帧都抛，不停的话日志会被刷爆，
        // 真正的第一条错误反而找不到了。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { update() { throw new Error('boom'); } };", "t.js");
        let id = runtime.instantiate(&script, NodeRef(0)).unwrap();

        runtime.tick(Snapshot::new(0.016, 0.0));
        assert!(runtime.is_failed(id));
        assert_eq!(runtime.stats().failed, 1);

        // 后续几帧不该再报错，也不该再跑。
        for _ in 0..3 {
            runtime.tick(Snapshot::new(0.016, 0.0));
            assert_eq!(runtime.stats().failed, 0);
            assert_eq!(runtime.stats().ran, 0);
        }
    }

    #[test]
    fn one_broken_script_does_not_stop_the_others() {
        let mut runtime = ScriptRuntime::new();
        runtime
            .instantiate(&Script::new("return { update() { throw 1; } };", "bad.js"), NodeRef(0))
            .unwrap();
        runtime
            .instantiate(
                &Script::new("return { update() { engine.log('fine'); } };", "good.js"),
                NodeRef(1),
            )
            .unwrap();

        let commands = runtime.tick(Snapshot::new(0.016, 0.0));

        assert_eq!(commands, vec![Command::Log("fine".into())]);
        assert_eq!(runtime.stats().ran, 1);
    }

    #[test]
    fn an_infinite_loop_errors_out_instead_of_hanging_the_engine() {
        // 这是整个脚本系统最要命的一条：没有这道闸，一个手滑的 `while(true)`
        // 就能把游戏彻底冻住，连日志都打不出来。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { update() { while (true) {} } };", "loop.js");
        let id = runtime.instantiate(&script, NodeRef(0)).unwrap();

        runtime.tick(Snapshot::new(0.016, 0.0));

        assert!(runtime.is_failed(id), "死循环没有被拦下");
    }

    #[test]
    fn a_command_flood_is_capped() {
        let mut runtime = ScriptRuntime::new().with_command_limit(10);
        let script = Script::new(
            "return { update() { for (let i = 0; i < 1000; i++) engine.log('x'); } };",
            "flood.js",
        );
        runtime.instantiate(&script, NodeRef(0)).unwrap();

        let commands = runtime.tick(Snapshot::new(0.016, 0.0));

        assert_eq!(commands.len(), 10);
        assert_eq!(runtime.stats().dropped, 990);
    }

    #[test]
    fn host_state_is_released_even_when_a_script_throws() {
        // 靠 `Drop` 收尾而不是手动收尾：抛异常时中间那段代码会被跳过，
        // 宿主状态留在那儿的话，下一帧会读到上一帧的快照。
        let mut runtime = ScriptRuntime::new();
        runtime
            .instantiate(&Script::new("return { update() { throw 1; } };", "t.js"), NodeRef(0))
            .unwrap();
        runtime.tick(two_nodes());

        // 新实例这一帧应当读到**新**快照。
        runtime
            .instantiate(
                &Script::new("return { update() { engine.log('' + engine.time()); } };", "t2.js"),
                NodeRef(0),
            )
            .unwrap();
        let commands = runtime.tick(Snapshot::new(0.016, 99.0));

        assert_eq!(commands, vec![Command::Log("99".into())]);
    }

    #[test]
    fn destroyed_slots_are_reused() {
        // 反复增删的场景里实例数不该无限涨。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { update() {} };", "t.js");

        for _ in 0..10 {
            let id = runtime.instantiate(&script, NodeRef(0)).unwrap();
            runtime.tick(Snapshot::new(0.016, 0.0));
            runtime.destroy(id, Snapshot::new(0.0, 0.0));
        }

        assert_eq!(runtime.instance_count(), 0);
        assert_eq!(runtime.instances.len(), 1, "槽位没有被复用");
    }

    #[test]
    fn rebinding_moves_a_script_to_a_new_snapshot_index() {
        // 节点在快照里的编号每帧都可能变（有节点增删时）。
        let mut runtime = ScriptRuntime::new();
        let script = Script::new("return { update() { engine.log(engine.name(engine.self())); } };", "t.js");
        let id = runtime.instantiate(&script, NodeRef(0)).unwrap();

        runtime.rebind(id, NodeRef(1));
        let commands = runtime.tick(two_nodes());

        assert_eq!(commands, vec![Command::Log("enemy".into())]);
    }

    #[test]
    fn scripts_run_in_a_stable_order() {
        let mut runtime = ScriptRuntime::new();
        for index in 0..5 {
            runtime
                .instantiate(
                    &Script::new(format!("return {{ update() {{ engine.emit('i', {index}); }} }};"), "t.js"),
                    NodeRef(0),
                )
                .unwrap();
        }

        let first = runtime.tick(Snapshot::new(0.016, 0.0));
        let second = runtime.tick(Snapshot::new(0.016, 0.0));

        assert_eq!(first, second, "两帧之间执行顺序变了");
    }
}
