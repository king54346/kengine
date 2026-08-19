//! 脚本组件：把 `kscript` 的脚本挂到场景节点上。
//!
//! # 一帧做三件事
//!
//! 1. **建快照**——把带脚本的节点（以及它们能查到的节点）拍成一份只读数据；
//! 2. **跑脚本**——`kscript` 在 VM 里执行，产出一串命令；
//! 3. **落地命令**——引擎按顺序应用，同一节点的多条命令按发出顺序生效。
//!
//! 运行时**不放在 `Scene` 里**，而是像 `AudioDevice` 一样由 `kapp` 持有：
//! boa 的 `Context` 不是 `Send`，塞进 `Scene` 会让整个场景失去跨线程能力，
//! 而场景的并行剔除正依赖这一点。
//!
//! # 快照里放谁
//!
//! **所有节点**都进快照，不只是带脚本的——脚本要能 `engine.find("player")`
//! 找到别人。代价是每帧一次 O(节点数) 的遍历；万级场景下这是实打实的开销，
//! 所以带脚本的场景应当控制规模，或者将来加一层「只快照标记过的节点」。
//! 现在诚实的做法是把这个代价写在这里，而不是假装它不存在。

use crate::{Node, Scene};
use kasset::Resource;
use kcore::pool::Handle;
use kmath::Quat;
use kscript::{Command, NodeRef, NodeState, Script, ScriptRuntime, Snapshot};

/// 挂在节点上的脚本。
#[derive(Debug, Clone)]
pub struct ScriptComponent {
    source: Resource<Script>,
    /// 运行时里对应的实例；还没实例化时为 [`None`]。
    instance: Option<kscript::InstanceId>,
    /// 是否参与执行。
    pub enabled: bool,
    /// 实例化失败过就不再重试——源码有语法错误的话，重试一万次也一样。
    failed: bool,
}

impl ScriptComponent {
    /// 用一份脚本资源创建。
    pub fn new(source: Resource<Script>) -> Self {
        Self {
            source,
            instance: None,
            enabled: true,
            failed: false,
        }
    }

    /// 脚本资源。
    pub fn source(&self) -> &Resource<Script> {
        &self.source
    }

    /// 运行时实例编号。
    pub fn instance(&self) -> Option<kscript::InstanceId> {
        self.instance
    }

    /// 实例化是否失败过。
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// 换一份脚本。旧实例会在下一次 tick 时被销毁重建。
    pub fn set_source(&mut self, source: Resource<Script>) {
        self.source = source;
        self.instance = None;
        self.failed = false;
    }
}

/// 脚本这一帧抛给游戏侧的一个事件。
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptEvent {
    /// 事件名。
    pub name: String,
    /// 附带的数值。
    pub value: f64,
    /// 发出它的节点。
    pub source: Handle<Node>,
}

impl Scene {
    /// 跑一帧脚本。
    ///
    /// 排在插件的 `update` 之后、物理之前：脚本改的是局部变换，
    /// 而物理与世界变换都要以它的结果为输入。
    ///
    /// 返回脚本抛出的事件，由调用方处理（见 [`ScriptEvent`]）。
    pub fn tick_scripts(
        &mut self,
        runtime: &mut ScriptRuntime,
        dt: f32,
        elapsed: f32,
    ) -> Vec<ScriptEvent> {
        // ── 1. 建快照 ──
        // 顺带记下「快照编号 → 节点句柄」，命令落地时要反查回去。
        let mut snapshot = Snapshot::new(dt, elapsed);
        let mut handles: Vec<Handle<Node>> = Vec::new();

        for index in 0..self.nodes.get_capacity() {
            let handle = self.nodes.handle_from_index(index);
            let Ok(node) = self.nodes.try_borrow(handle) else {
                continue;
            };
            snapshot.push(NodeState {
                name: node.name.clone(),
                position: node.transform.position,
                rotation: node.transform.rotation,
                scale: node.transform.scale,
                world_position: node.global_transform.w_axis.truncate(),
                visible: node.visible,
            });
            handles.push(handle);
        }

        // ── 2. 实例化新脚本、刷新已有脚本的节点编号 ──
        for slot in 0..handles.len() {
            let handle = handles[slot];
            let node_ref = NodeRef(slot as u32);

            let Ok(node) = self.nodes.try_borrow_mut(handle) else {
                continue;
            };
            let Some(component) = node.script.as_deref_mut() else {
                continue;
            };
            if !component.enabled || component.failed {
                continue;
            }

            match component.instance {
                Some(instance) => runtime.rebind(instance, node_ref),
                None => {
                    // 资源还没加载完就等下一帧——脚本是异步加载的。
                    let Some(script) = component.source.data_ref().map(|data| data.clone()) else {
                        continue;
                    };
                    match runtime.instantiate(&script, node_ref) {
                        Some(instance) => component.instance = Some(instance),
                        None => component.failed = true,
                    }
                }
            }
        }

        // ── 3. 跑，然后落地 ──
        let commands = runtime.tick(snapshot);
        self.apply_commands(&commands, &handles)
    }

    /// 把脚本发出的命令应用到场景上。
    fn apply_commands(
        &mut self,
        commands: &[Command],
        handles: &[Handle<Node>],
    ) -> Vec<ScriptEvent> {
        let mut events = Vec::new();
        let resolve = |node: NodeRef| -> Option<Handle<Node>> {
            handles.get(node.0 as usize).copied()
        };

        for command in commands {
            match command {
                Command::Log(text) => klog::info!("[脚本] {text}"),
                Command::Emit {
                    name,
                    value,
                    source,
                } => events.push(ScriptEvent {
                    name: name.clone(),
                    value: *value,
                    // 来源是脚本发出事件的那一刻记下的，不是事后猜的。
                    source: resolve(*source).unwrap_or(Handle::NONE),
                }),
                Command::Despawn(node) => {
                    if let Some(handle) = resolve(*node) {
                        self.remove_node(handle);
                    }
                }
                _ => {
                    let Some(handle) = command.target().and_then(resolve) else {
                        continue;
                    };
                    // 节点可能在同一帧里被前面的命令删掉了，这不是错误。
                    let Ok(node) = self.nodes.try_borrow_mut(handle) else {
                        continue;
                    };

                    match command {
                        Command::SetPosition(_, value) => node.transform.position = *value,
                        Command::Translate(_, value) => node.transform.position += *value,
                        Command::SetRotation(_, value) => node.transform.rotation = *value,
                        Command::RotateY(_, angle) => {
                            node.transform.rotation *= Quat::from_rotation_y(*angle)
                        }
                        Command::SetScale(_, value) => node.transform.scale = *value,
                        Command::SetVisible(_, visible) => node.visible = *visible,
                        Command::ApplyImpulse(_, impulse) => {
                            if let Some(body) = node.rigid_body_mut() {
                                body.apply_impulse(*impulse);
                            }
                        }
                        Command::PlaySound(_) => {
                            if let Some(sound) = node.sound_mut() {
                                sound.restart();
                            }
                        }
                        // 上面已经处理过。
                        Command::Log(_) | Command::Emit { .. } | Command::Despawn(_) => {}
                    }
                }
            }
        }

        events
    }

    /// 销毁场景里所有脚本实例。
    ///
    /// 切场景时要调，否则运行时里会留下一堆没有主人的实例——
    /// 它们仍然会每帧执行，还会对着过期的节点编号发命令。
    pub fn destroy_scripts(&mut self, runtime: &mut ScriptRuntime) {
        let snapshot = Snapshot::new(0.0, 0.0);
        for index in 0..self.nodes.get_capacity() {
            let handle = self.nodes.handle_from_index(index);
            let Ok(node) = self.nodes.try_borrow_mut(handle) else {
                continue;
            };
            let Some(component) = node.script.as_deref_mut() else {
                continue;
            };
            if let Some(instance) = component.instance.take() {
                runtime.destroy(instance, snapshot.clone());
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Collider, RigidBody};
    use kasset::{MemoryResourceIo, ResourceManager};
    use kmesh::Mesh;
    use kmath::Vec3;
    use kscript::ScriptLoader;
    use std::sync::Arc;

    /// 一个装好脚本加载器、内存里放着几个脚本的资源管理器。
    fn manager() -> ResourceManager {
        let io = MemoryResourceIo::new()
            .with(
                "spin.js",
                b"return { update(dt) { engine.rotateY(engine.self(), dt); } };".to_vec(),
            )
            .with(
                "move.js",
                b"return { update(dt) { engine.translate(engine.self(), dt, 0, 0); } };".to_vec(),
            )
            .with("bad.js", b"return { this is not js };".to_vec());

        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(ScriptLoader);
        manager
    }

    fn script(manager: &ResourceManager, path: &str) -> Resource<Script> {
        manager.request_blocking::<Script>(path).unwrap()
    }

    #[test]
    fn a_script_moves_its_own_node() {
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("mover").with_script(ScriptComponent::new(script(&manager, "move.js"))),
        );

        scene.update();
        scene.tick_scripts(&mut runtime, 0.5, 0.0);

        assert_eq!(scene[node].transform.position, Vec3::new(0.5, 0.0, 0.0));
    }

    #[test]
    fn a_script_accumulates_across_frames() {
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("mover").with_script(ScriptComponent::new(script(&manager, "move.js"))),
        );
        scene.update();

        for _ in 0..4 {
            scene.tick_scripts(&mut runtime, 0.25, 0.0);
            scene.update();
        }

        assert!((scene[node].transform.position.x - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rotation_commands_compose() {
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("spinner").with_script(ScriptComponent::new(script(&manager, "spin.js"))),
        );
        scene.update();

        for _ in 0..2 {
            scene.tick_scripts(&mut runtime, 0.5, 0.0);
        }

        // 转过 1 弧度，前向应当明显偏离初始的 -Z。
        let forward = scene[node].transform.rotation * Vec3::NEG_Z;
        assert!(forward.x.abs() > 0.5, "没有累积旋转：{forward:?}");
    }

    #[test]
    fn scripts_can_find_and_move_other_nodes() {
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new().with(
            "push.js",
            b"return { update() { engine.setPosition(engine.find('target'), 5, 0, 0); } };".to_vec(),
        )));
        manager.add_loader(ScriptLoader);

        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("driver").with_script(ScriptComponent::new(script(&manager, "push.js"))),
        );
        let target = scene.add_node(Node::new("target"));

        scene.update();
        scene.tick_scripts(&mut runtime, 0.016, 0.0);

        assert_eq!(scene[target].transform.position, Vec3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn a_script_reads_the_world_position_from_the_last_update() {
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new().with(
            "read.js",
            b"return { update() { engine.emit('y', engine.worldPosition(engine.self()).y); } };"
                .to_vec(),
        )));
        manager.add_loader(ScriptLoader);

        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let parent = scene.add_node(Node::new("parent").with_position(Vec3::Y * 10.0));
        scene.add_node_with_parent(
            Node::new("child")
                .with_position(Vec3::Y * 3.0)
                .with_script(ScriptComponent::new(script(&manager, "read.js"))),
            parent,
        );
        scene.update();

        let events = scene.tick_scripts(&mut runtime, 0.016, 0.0);

        assert_eq!(events.len(), 1);
        assert!((events[0].value - 13.0).abs() < 1e-4, "世界坐标不对：{}", events[0].value);
    }

    #[test]
    fn a_script_can_apply_an_impulse_to_a_rigid_body() {
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new().with(
            "jump.js",
            b"return { init() { engine.applyImpulse(engine.self(), 0, 10, 0); }, update() {} };"
                .to_vec(),
        )));
        manager.add_loader(ScriptLoader);

        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("ball")
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::ball(0.5))
                .with_script(ScriptComponent::new(script(&manager, "jump.js"))),
        );

        scene.update();
        scene.tick_scripts(&mut runtime, 1.0 / 60.0, 0.0);
        scene.step_physics(1.0 / 60.0);

        assert!(
            scene[node].rigid_body().unwrap().linvel().y > 1.0,
            "冲量没有传到刚体"
        );
    }

    #[test]
    fn a_script_can_despawn_a_node() {
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new().with(
            "kill.js",
            b"return { update() { engine.despawn(engine.find('victim')); } };".to_vec(),
        )));
        manager.add_loader(ScriptLoader);

        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("killer").with_script(ScriptComponent::new(script(&manager, "kill.js"))),
        );
        let victim = scene.add_node(Node::new("victim").with_mesh(Mesh::cube()));

        scene.update();
        scene.tick_scripts(&mut runtime, 0.016, 0.0);
        scene.update();

        assert!(scene.try_get(victim).is_none());
        assert_eq!(scene.drawable_count(), 0);
    }

    #[test]
    fn commands_aimed_at_an_already_despawned_node_are_ignored() {
        // 同一帧里前面的命令可能已经把节点删了，这不是错误。
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new().with(
            "double.js",
            b"return { update() {
                let v = engine.find('victim');
                engine.despawn(v);
                engine.setPosition(v, 1, 1, 1);
            } };"
                .to_vec(),
        )));
        manager.add_loader(ScriptLoader);

        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("driver").with_script(ScriptComponent::new(script(&manager, "double.js"))),
        );
        scene.add_node(Node::new("victim"));

        scene.update();
        scene.tick_scripts(&mut runtime, 0.016, 0.0);
    }

    #[test]
    fn events_reach_the_game_side() {
        let manager = ResourceManager::with_io(Arc::new(MemoryResourceIo::new().with(
            "emit.js",
            b"return { update() { engine.emit('score', 42); } };".to_vec(),
        )));
        manager.add_loader(ScriptLoader);

        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("scorer").with_script(ScriptComponent::new(script(&manager, "emit.js"))),
        );
        scene.update();

        let events = scene.tick_scripts(&mut runtime, 0.016, 0.0);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "score");
        assert_eq!(events[0].value, 42.0);
        assert_eq!(events[0].source, node);
    }

    #[test]
    fn a_broken_script_is_marked_and_not_retried() {
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("broken").with_script(ScriptComponent::new(script(&manager, "bad.js"))),
        );
        scene.update();

        scene.tick_scripts(&mut runtime, 0.016, 0.0);
        assert!(scene[node].script().unwrap().is_failed());

        // 后续几帧不该再尝试实例化。
        for _ in 0..3 {
            scene.tick_scripts(&mut runtime, 0.016, 0.0);
        }
        assert_eq!(runtime.instance_count(), 0);
    }

    #[test]
    fn a_disabled_script_does_not_run() {
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("mover").with_script(ScriptComponent::new(script(&manager, "move.js"))),
        );
        scene[node].script_mut().unwrap().enabled = false;
        scene.update();

        scene.tick_scripts(&mut runtime, 0.5, 0.0);

        assert_eq!(scene[node].transform.position, Vec3::ZERO);
    }

    #[test]
    fn a_script_whose_resource_is_still_loading_waits() {
        // 脚本资源是异步加载的，挂上去那一帧未必已经就绪。
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let missing = manager.request::<Script>("nope.js");
        let node = scene.add_node(Node::new("later").with_script(ScriptComponent::new(missing)));
        scene.update();

        for _ in 0..3 {
            scene.tick_scripts(&mut runtime, 0.016, 0.0);
        }

        assert_eq!(runtime.instance_count(), 0);
        assert!(!scene[node].script().unwrap().is_failed(), "没就绪不等于失败");
    }

    #[test]
    fn each_node_gets_its_own_script_instance() {
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        let a = scene.add_node(
            Node::new("a").with_script(ScriptComponent::new(script(&manager, "move.js"))),
        );
        let b = scene.add_node(
            Node::new("b").with_script(ScriptComponent::new(script(&manager, "move.js"))),
        );
        scene.update();

        scene.tick_scripts(&mut runtime, 0.5, 0.0);

        assert_eq!(runtime.instance_count(), 2);
        assert_eq!(scene[a].transform.position.x, 0.5);
        assert_eq!(scene[b].transform.position.x, 0.5);
    }

    #[test]
    fn destroying_scripts_clears_the_runtime() {
        // 切场景时不清的话，运行时里会留下一堆没有主人的实例，
        // 它们仍然每帧执行，还会对着过期的节点编号发命令。
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();
        for index in 0..3 {
            scene.add_node(
                Node::new(format!("n{index}"))
                    .with_script(ScriptComponent::new(script(&manager, "move.js"))),
            );
        }
        scene.update();
        scene.tick_scripts(&mut runtime, 0.016, 0.0);
        assert_eq!(runtime.instance_count(), 3);

        scene.destroy_scripts(&mut runtime);

        assert_eq!(runtime.instance_count(), 0);
    }

    #[test]
    fn scripts_survive_nodes_being_added_and_removed() {
        // 节点在快照里的编号每帧都会变，绑定没刷新的话脚本会开始操作别人。
        let manager = manager();
        let mut runtime = ScriptRuntime::new();
        let mut scene = Scene::new();

        let filler = scene.add_node(Node::new("filler"));
        let mover = scene.add_node(
            Node::new("mover").with_script(ScriptComponent::new(script(&manager, "move.js"))),
        );
        scene.update();
        scene.tick_scripts(&mut runtime, 0.5, 0.0);
        assert_eq!(scene[mover].transform.position.x, 0.5);

        // 把前面的节点删掉，编号整体前移。
        scene.remove_node(filler);
        scene.update();
        scene.tick_scripts(&mut runtime, 0.5, 0.0);

        assert_eq!(scene[mover].transform.position.x, 1.0, "脚本操作错了节点");
    }

    #[test]
    fn script_driven_motion_is_deterministic() {
        fn run() -> Vec3 {
            let manager = manager();
            let mut runtime = ScriptRuntime::new();
            let mut scene = Scene::new();
            let node = scene.add_node(
                Node::new("mover").with_script(ScriptComponent::new(script(&manager, "move.js"))),
            );
            scene.update();
            for _ in 0..30 {
                scene.tick_scripts(&mut runtime, 1.0 / 60.0, 0.0);
                scene.update();
            }
            scene[node].transform.position
        }

        assert_eq!(run(), run());
    }
}
