//! 用 kengine 写的示例游戏。
//!
//! 引擎负责窗口、事件循环、输入采集和渲染；这个文件里只有游戏逻辑。
//!
//! 操作：WASD 移动方块，Q/E 升降，空格暂停自转，R 重置，C 打印统计，
//! F 喷一团火花，1/2/3 切换士兵的动作，M 让狮子闭嘴/张嘴，Esc 退出。
//!
//! 加 `--stress N` 可以再铺 N 个共享网格的方块，用来观察空间划分、
//! 并行剔除与实例化在万级对象下的表现：
//!
//! ```text
//! cargo run --release -- --stress 20000
//! ```

use kengine::prelude::*;
use kcore::uuid::{Uuid, uuid};
use std::{path::PathBuf, sync::Arc};

/// 一个自定义资源类型：从文本文件读出的关卡配置。
#[derive(Debug)]
struct LevelConfig {
    spin_speed: f32,
    move_speed: f32,
}

impl ResourceData for LevelConfig {
    fn type_uuid(&self) -> Uuid {
        uuid!("6b1e4d90-2a73-4c58-8f21-9d3c7e5a0b64")
    }
}

/// 对应的加载器：解析 `key = value` 形式的文本。
#[derive(Debug)]
struct LevelConfigLoader;

impl ResourceLoader for LevelConfigLoader {
    fn extensions(&self) -> &[&str] {
        &["level"]
    }

    fn data_type_uuid(&self) -> Uuid {
        uuid!("6b1e4d90-2a73-4c58-8f21-9d3c7e5a0b64")
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let text = String::from_utf8(bytes).map_err(LoadError::custom)?;

            let mut config = LevelConfig {
                spin_speed: 1.0,
                move_speed: 2.0,
            };
            for line in text.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value: f32 = value.trim().parse().map_err(LoadError::custom)?;
                match key.trim() {
                    "spin_speed" => config.spin_speed = value,
                    "move_speed" => config.move_speed = value,
                    _ => {}
                }
            }
            Ok(Box::new(config) as Box<dyn ResourceData>)
        })
    }
}

#[derive(Default)]
struct Game {
    /// `--stress N` 指定的额外方块数。
    stress: usize,
    cube: Handle<Node>,
    orbit: Handle<Node>,
    config: Option<Resource<LevelConfig>>,
    /// 异步加载的 glTF 模型，就绪后再实例化进场景。
    model: Option<Resource<Model>>,
    model_spawned: bool,
    stats_reported: bool,
    /// 下一次自动汇报统计的时刻。
    report_at: f32,
    /// 绕场景转圈的点光源，用来观察多光源与衰减。
    lamp: Handle<Node>,
    /// 相加混合的火花，按 F 手动喷一团。
    sparks: Handle<Node>,
    /// 异步加载的骨骼动画模型。
    soldier: Option<Resource<Model>>,
    soldier_node: Handle<Node>,
    soldier_spawned: bool,
    /// 驱动士兵动作的状态机与参数。
    locomotion: Option<StateMachine>,
    parameters: Parameters,
    /// 目标移动速度，1/2/3 键切换；状态机据此选动作。
    target_speed: f32,
    /// 异步加载的形变目标模型。
    lion: Option<Resource<Model>>,
    lion_spawned: bool,
    /// 带各个形变的节点：嘴、左眼、右眼、舌头。
    lion_morphs: Vec<(Handle<Node>, String)>,
    /// 形变是否自动张合。
    lion_talking: bool,
    /// 物理演示：箱子堆的容器节点，X 键整摞重来。
    crates_root: Handle<Node>,
    /// 运动学电梯平台。
    elevator: Handle<Node>,
    /// 传感器触发区。
    trigger: Handle<Node>,
    /// 士兵的布娃娃节点，K 键切换。
    ragdoll: Handle<Node>,
    /// 重力是否开着，G 键切换。
    gravity_on: bool,
    /// `--roundtrip`：自动做一次存盘 → 读档，并对比前后的渲染统计。
    roundtrip: bool,
    /// 自检的第几步。
    roundtrip_step: u8,
    /// 存盘那一刻的渲染统计，用来和读回来之后对比。
    stats_before: Option<RenderStats>,
    /// 循环播放的精灵。
    sprite_loop: SpriteAnimation,
    sprite_loop_node: Handle<Node>,
    /// 来回播放的精灵。
    sprite_ping: SpriteAnimation,
    sprite_ping_node: Handle<Node>,
    /// 绕圈的 3D 声源。
    audio_orbit: Handle<Node>,
    /// B 键放的一次性提示音。资源在 `init` 里登记，之前为 `None`。
    beep_sound: Option<Resource<AudioBuffer>>,
    /// 正在播的一次性音效节点，播完就清掉。
    beep_nodes: Vec<Handle<Node>>,
    /// 被 JS 驱动的方块。
    script_spinner: Handle<Node>,
    paused: bool,
}

/// 物理场地的原点。整体挪到 +X 一侧，免得和 PBR 参数球、粒子挤在一起。
const PLAYGROUND: Vec3 = Vec3::new(7.0, 0.0, 0.0);

impl Game {
    /// 铺一片共享同一份网格与材质的方块，用来压测剔除与批处理。
    ///
    /// 网格与材质都是克隆的：网格克隆共享同一个 id，显存只占一份；
    /// 贴图槽也完全一样，于是这一整片方块能合并成**一次**绘制调用。
    fn spawn_stress_field(&mut self, ctx: &mut Context) {
        if self.stress == 0 {
            return;
        }

        let mesh = Mesh::cube();
        let material = PbrMaterial::dielectric(Vec3::new(0.7, 0.7, 0.75), 0.6);
        // 排成一片正方形网格，边长按数量开方推出来。
        let side = (self.stress as f32).sqrt().ceil() as usize;
        let spacing = 1.8;
        let origin = -(side as f32) * spacing * 0.5;

        for index in 0..self.stress {
            let (x, z) = (index % side, index / side);
            // 高度错开一点，免得整片方块共面、包围盒退化成一张纸。
            let height = ((x * 7 + z * 13) % 5) as f32 * 0.25;
            ctx.scene.add_node(
                Node::new("Stress")
                    .with_mesh(mesh.clone())
                    .with_material(material.clone())
                    .with_position(Vec3::new(
                        origin + x as f32 * spacing,
                        -0.8 + height,
                        origin + z as f32 * spacing,
                    ))
                    .with_scale(Vec3::splat(0.6)),
            );
        }

        klog::info!("压力测试：额外生成 {} 个方块", self.stress);
    }

    /// 士兵就绪的那一帧：实例化进场景，并搭一个 Idle ⇄ Walk ⇄ Run 状态机。
    fn spawn_soldier(&mut self, ctx: &mut Context) {
        let Some(model) = self.soldier.as_ref().and_then(Resource::data_ref) else {
            return;
        };
        self.soldier_spawned = true;
        // 士兵进场后再汇报一次统计，好确认蒙皮那条管线确实在画东西。
        // 统计读到的是上一帧的结果，所以要等一帧之后再看。
        self.stats_reported = false;
        self.report_at = ctx.elapsed + 1.0;

        let root = ctx.scene.root();
        self.soldier_node = ctx.scene.instantiate_model(&model, root);
        ctx.scene[self.soldier_node].transform.position = Vec3::new(0.0, -1.0, -1.0);

        // 剪辑序号要问播放器要——状态机记的是序号，不是名字。
        let Some(player) = ctx.scene[self.soldier_node].animator() else {
            klog::warn!("士兵模型没有动画");
            return;
        };
        let animator = player.animator();
        let (Some(idle), Some(walk), Some(run)) = (
            animator.clip_index("Idle"),
            animator.clip_index("Walk"),
            animator.clip_index("Run"),
        ) else {
            klog::warn!("士兵模型缺少 Idle/Walk/Run 之一");
            return;
        };

        let mut machine = StateMachine::new();
        let idle_state = machine.add_state(State::clip("Idle", idle));
        // 移动状态内部是一维混合空间：速度在走与跑之间连续过渡，
        // 而不是到某个阈值突然换一套动作。
        let moving = machine.add_state(State::new(
            "Moving",
            BlendTree::blend_space_1d(
                "speed",
                [(1.0, BlendTree::Clip(walk)), (4.0, BlendTree::Clip(run))],
            ),
        ));
        machine.add_transition(Transition::new(
            idle_state,
            moving,
            0.25,
            Condition::greater("speed", 0.1),
        ));
        machine.add_transition(Transition::new(
            moving,
            idle_state,
            0.25,
            Condition::less("speed", 0.1),
        ));

        self.locomotion = Some(machine);
        klog::info!("士兵已载入：{} 个动画，按 1/2/3 切换站立/行走/奔跑", animator.clips().len());
    }

    /// 每帧把状态机算出的权重交给播放器。
    fn drive_soldier(&mut self, ctx: &mut Context) {
        let Some(machine) = self.locomotion.as_mut() else {
            return;
        };

        // 速度平滑过渡，免得混合空间的坐标突跳。
        let current = self.parameters.float("speed");
        let speed = current + (self.target_speed - current) * (ctx.dt * 6.0).min(1.0);
        self.parameters.set_float("speed", speed);

        machine.update(ctx.dt, &self.parameters);
        let weights = machine.weights(&self.parameters);

        let Some(player) = ctx.scene[self.soldier_node].animator_mut() else {
            return;
        };
        player.animator_mut().apply_weights(&weights);
    }

    /// 狮子就绪的那一帧：实例化进场景，并记下带形变的节点。
    fn spawn_lion(&mut self, ctx: &mut Context) {
        let Some(model) = self.lion.as_ref().and_then(Resource::data_ref) else {
            return;
        };
        self.lion_spawned = true;
        self.lion_talking = true;

        let root = ctx.scene.root();
        let handle = ctx.scene.instantiate_model(&model, root);

        // 形变分散在几个网格上（嘴、眼睛、舌头各一个），逐个记下来。
        let mut stack = vec![handle];
        while let Some(node) = stack.pop() {
            stack.extend_from_slice(ctx.scene[node].children());
            let Some(mesh) = ctx.scene[node].mesh() else {
                continue;
            };
            for target in mesh.morph_targets() {
                self.lion_morphs.push((node, target.name().to_string()));
            }
        }

        let names: Vec<&str> = self
            .lion_morphs
            .iter()
            .map(|(_, name)| name.as_str())
            .collect();
        // 模型自带的坐标尺度五花八门（这头狮子的局部坐标就在几十的量级上），
        // 所以先量出它的实际范围，再按需要的大小和位置摆过去。
        ctx.scene.update();
        let mut bounds = Aabb::EMPTY;
        let mut stack = vec![handle];
        while let Some(node) = stack.pop() {
            stack.extend_from_slice(ctx.scene[node].children());
            let aabb = ctx.scene[node].global_aabb();
            if !aabb.is_empty() {
                bounds = bounds.union(&aabb);
            }
        }

        if !bounds.is_empty() {
            const TARGET_HEIGHT: f32 = 2.2;
            let scale = TARGET_HEIGHT / bounds.size().max_element().max(1e-6);
            let target = Vec3::new(3.4, 0.6, -1.0);
            ctx.scene[handle].transform.scale = Vec3::splat(scale);
            // 量到的中心是缩放前的，摆位时要按同样的比例折算回去。
            ctx.scene[handle].transform.position = target - bounds.center() * scale;
        }

        klog::info!(
            "狮子已载入：{} 个形变目标 {:?}，按 M 开关自动张合",
            names.len(),
            names
        );
    }

    /// 让狮子的形变权重随时间起伏。
    fn drive_lion(&mut self, ctx: &mut Context) {
        if !self.lion_talking || self.lion_morphs.is_empty() {
            return;
        }

        for (index, (handle, name)) in self.lion_morphs.iter().enumerate() {
            // 每个形变错开相位，看起来像在说话、眨眼。
            let phase = ctx.elapsed * 2.0 + index as f32 * 1.3;
            let weight = (phase.sin() * 0.5 + 0.5).clamp(0.0, 1.0);
            let Some(node) = ctx.scene.try_get_mut(*handle) else {
                continue;
            };
            node.set_morph_weight_by_name(name, weight);
        }
    }

    /// 打印一帧的渲染统计。

    // ───────────────────────── 物理演示 ─────────────────────────

    /// 搭一片物理场地：地面、一摞箱子、一个单摆、一部电梯、一个触发区。
    ///
    /// 场地整体挪到 +X 一侧，免得和 PBR 参数球、粒子挤在一起。
    fn spawn_physics_playground(&mut self, ctx: &mut Context) {
        // 地面：可见的是一张平面网格，物理这边配一块厚板，上表面对齐 y = -1。
        ctx.scene.add_node(
            Node::new("PhysicsGround")
                .with_position(Vec3::new(0.0, -1.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::new(
                    ColliderDesc::cuboid(Vec3::new(40.0, 0.5, 40.0)).with_material(0.8, 0.0),
                )),
        );

        // 箱子堆：一个空节点当容器，重置时整棵删掉重建。
        self.crates_root = ctx.scene.add_node(Node::new("Crates"));
        self.reset_crates(ctx);

        // 单摆：固定锚点 + 铰链关节 + 一颗重球。绕 Z 轴转，只在 XZ 之外的竖直平面里荡。
        let anchor = ctx.scene.add_node(
            Node::new("PendulumAnchor")
                .with_position(PLAYGROUND + Vec3::new(0.0, 4.0, -3.0))
                .with_rigid_body(RigidBody::fixed()),
        );
        let bob = ctx.scene.add_node(
            Node::new("PendulumBob")
                .with_mesh(Mesh::sphere(20, 28))
                .with_material(PbrMaterial::metal(Vec3::new(0.9, 0.85, 0.5), 0.2))
                .with_position(PLAYGROUND + Vec3::new(2.5, 4.0, -3.0))
                .with_scale(Vec3::splat(0.5))
                .with_rigid_body(RigidBody::new(
                    // 重一点才推得动箱子。
                    RigidBodyDesc::dynamic().with_additional_mass(20.0),
                ))
                .with_collider(Collider::ball(0.5)),
        );
        ctx.scene.add_node(Node::new("PendulumJoint").with_joint(Joint::new(
            anchor,
            bob,
            JointDesc::revolute(Vec3::ZERO, Vec3::new(-2.5, 0.0, 0.0), Vec3::Z, None),
        )));

        // 电梯：运动学刚体，位置每帧由 `drive_physics` 写死，不受碰撞影响。
        self.elevator = ctx.scene.add_node(
            Node::new("Elevator")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::dielectric(Vec3::new(0.2, 0.6, 0.9), 0.5))
                .with_position(PLAYGROUND + Vec3::new(-3.0, -0.75, 2.0))
                .with_scale(Vec3::new(2.0, 0.3, 2.0))
                .with_rigid_body(RigidBody::kinematic())
                // 碰撞体的尺寸是它自己的参数，与节点的缩放无关，得手写半长。
                .with_collider(Collider::cuboid(Vec3::new(1.0, 0.15, 1.0))),
        );

        // 触发区：只报告重叠，不产生碰撞响应。箱子被撞飞落进来时会打日志。
        self.trigger = ctx.scene.add_node(
            Node::new("Trigger")
                .with_position(PLAYGROUND + Vec3::new(4.5, -0.2, 2.5))
                .with_collider(Collider::new(
                    ColliderDesc::cuboid(Vec3::new(1.2, 0.8, 1.2))
                        .as_sensor()
                        .with_collision_events(),
                )),
        );
    }

    /// 重新码一摞箱子。旧的整棵删掉——`remove_node` 会连带清掉物理世界里的对应物。
    fn reset_crates(&mut self, ctx: &mut Context) {
        let root = self.crates_root;
        let children: Vec<_> = ctx.scene[root].children().to_vec();
        for child in children {
            ctx.scene.remove_node(child);
        }

        let mesh = Mesh::cube();
        let material = PbrMaterial::dielectric(Vec3::new(0.72, 0.45, 0.24), 0.75);
        const LEVELS: usize = 5;
        for level in 0..LEVELS {
            // 每层比下一层少一个，码成金字塔，塌下来比较好看。
            let count = LEVELS - level;
            for index in 0..count {
                let x = (index as f32 - (count - 1) as f32 / 2.0) * 0.62;
                ctx.scene.add_node_with_parent(
                    Node::new("Crate")
                        .with_mesh(mesh.clone())
                        .with_material(material.clone())
                        .with_position(PLAYGROUND + Vec3::new(x, -0.7 + level as f32 * 0.62, -3.0))
                        .with_scale(Vec3::splat(0.3))
                        .with_rigid_body(RigidBody::dynamic())
                        // 碰撞体尺寸独立于节点缩放：立方体网格边长 1，缩放 0.3 后
                        // 半长是 0.15。
                        .with_collider(Collider::new(
                            ColliderDesc::cuboid(Vec3::splat(0.3))
                                .with_material(0.7, 0.0)
                                .with_collision_events(),
                        )),
                    root,
                );
            }
        }
    }

    /// 从相机往正前方打一条射线，命中什么就推什么，同时发射一颗炮弹。
    fn shoot(&mut self, ctx: &mut Context) {
        let Some((camera_world, _)) = ctx.scene.active_camera() else {
            return;
        };
        let origin = camera_world.w_axis.truncate();
        // 相机看向自己的 -Z，这是本引擎（与 glTF）的约定。
        let forward = -camera_world.z_axis.truncate().normalize_or_zero();

        // 先看看瞄到了什么。射线不修改任何状态，纯查询。
        if let Some(hit) = ctx
            .scene
            .cast_ray(&RayCastOptions::new(origin, forward, 100.0))
        {
            let name = hit
                .body_node
                .or(hit.collider_node)
                .and_then(|h| ctx.scene.try_get(h))
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "?".to_string());
            klog::info!("射线命中「{}」，距离 {:.2}", name, hit.distance);

            // 命中动态刚体就在命中点推一把——偏离质心，物体会转起来。
            if let Some(node) = hit.body_node.and_then(|h| ctx.scene.try_get_mut(h))
                && let Some(body) = node.rigid_body_mut()
                && body.body_type() == RigidBodyType::Dynamic
            {
                body.apply_impulse_at_point(forward * 6.0, hit.point);
            }
        }

        // 再发一颗炮弹，直观看得见冲量。
        ctx.scene.add_node(
            Node::new("Projectile")
                .with_mesh(Mesh::sphere(12, 16))
                .with_material(PbrMaterial::emissive(
                    Vec3::new(0.9, 0.3, 0.2),
                    Vec3::new(2.5, 0.6, 0.2),
                ))
                .with_position(origin + forward * 1.2)
                .with_scale(Vec3::splat(0.2))
                .with_rigid_body(RigidBody::new(
                    RigidBodyDesc::dynamic()
                        .with_linvel(forward * 24.0)
                        // 小而快的东西最容易穿墙，这正是 CCD 的用武之地。
                        .with_ccd(true),
                ))
                .with_collider(Collider::new(
                    ColliderDesc::ball(0.2).with_material(0.4, 0.4).with_density(6.0),
                )),
        );
    }

    /// 给士兵搭一套人形布娃娃。找不到骨骼就安静跳过。
    fn build_soldier_ragdoll(&mut self, ctx: &mut Context) {
        // Mixamo 的骨骼命名，士兵模型用的就是这一套。
        let bone = |ctx: &Context, name: &str| {
            ctx.scene
                .find_by_name(&format!("mixamorig:{name}"))
                .unwrap_or(Handle::NONE)
        };

        let hips = bone(ctx, "Hips");
        if hips.is_none() {
            return;
        }

        // 一节肢体：一段胶囊，沿骨头方向（士兵骨架里是 +Y）挪半个长度，
        // 这样胶囊裹住的是骨头本身而不是骨头上方。
        let limb = |handle: Handle<Node>, half: f32, radius: f32| {
            LimbDesc::new(handle, ColliderShape::capsule_y(half, radius))
                .with_offset(Vec3::new(0.0, half, 0.0), Quat::IDENTITY)
        };
        // 肩、髋这类球窝关节限位收得比较紧，松了胳膊会向后翻过去。
        let ball = |half_angle: f32| JointDesc {
            kind: JointKind::Spherical {
                limits: SphericalLimits::symmetric(half_angle),
            },
            ..Default::default()
        };
        // 肘、膝只朝一个方向弯。
        let hinge = |min: f32, max: f32| JointDesc {
            kind: JointKind::Revolute {
                axis: Vec3::X,
                limits: hinge_limits(min, max),
            },
            ..Default::default()
        };

        let arm = |side: &str| {
            limb(bone(ctx, &format!("{side}Arm")), 0.13, 0.06)
                .with_joint(ball(1.0))
                .with_child(
                    limb(bone(ctx, &format!("{side}ForeArm")), 0.12, 0.05)
                        .with_joint(hinge(-2.2, 0.0))
                        .with_child(
                            limb(bone(ctx, &format!("{side}Hand")), 0.05, 0.04)
                                .with_joint(ball(0.5)),
                        ),
                )
        };
        let leg = |side: &str| {
            limb(bone(ctx, &format!("{side}UpLeg")), 0.2, 0.08)
                .with_joint(ball(0.8))
                .with_child(
                    limb(bone(ctx, &format!("{side}Leg")), 0.2, 0.07)
                        .with_joint(hinge(0.0, 2.2))
                        .with_child(
                            limb(bone(ctx, &format!("{side}Foot")), 0.06, 0.05)
                                .with_joint(hinge(-0.6, 0.6)),
                        ),
                )
        };

        let skeleton = limb(hips, 0.1, 0.12)
            .with_child(
                limb(bone(ctx, "Spine1"), 0.12, 0.11)
                    .with_joint(ball(0.4))
                    .with_child(
                        limb(bone(ctx, "Spine2"), 0.12, 0.11)
                            .with_joint(ball(0.4))
                            .with_child(limb(bone(ctx, "Head"), 0.1, 0.09).with_joint(ball(0.6)))
                            .with_child(arm("Left"))
                            .with_child(arm("Right")),
                    ),
            )
            .with_child(leg("Left"))
            .with_child(leg("Right"));

        let root = ctx.scene.root();
        self.ragdoll = RagdollBuilder::new(skeleton)
            .with_name("SoldierRagdoll")
            .build(ctx.scene, root);

        klog::info!("士兵布娃娃已就绪（{} 节肢体），按 K 切换", {
            ctx.scene
                .try_get(self.ragdoll)
                .and_then(Node::ragdoll)
                .map(Ragdoll::limb_count)
                .unwrap_or(0)
        });
    }

    /// 每帧的物理相关逻辑：电梯、按键、触发区日志。
    fn drive_physics(&mut self, ctx: &mut Context) {
        // 电梯：直接写节点位置。运动学刚体就是这么驱动的，
        // 它会推开挡路的箱子，自己却纹丝不动。
        if self.elevator.is_some() {
            let height = -0.75 + (ctx.elapsed * 0.8).sin() * 1.2 + 1.2;
            ctx.scene[self.elevator].transform.position =
                PLAYGROUND + Vec3::new(-3.0, height, 2.0);
        }

        if ctx.input.action_just_pressed("shoot") {
            self.shoot(ctx);
        }

        if ctx.input.action_just_pressed("save") {
            Self::save_scene(ctx);
        }
        if ctx.input.action_just_pressed("load") {
            self.load_scene(ctx);
        }

        if ctx.input.action_just_pressed("restack") {
            self.reset_crates(ctx);
            klog::info!("箱子已重新码好");
        }

        if ctx.input.action_just_pressed("gravity") {
            self.gravity_on = !self.gravity_on;
            let gravity = if self.gravity_on {
                Vec3::new(0.0, -9.81, 0.0)
            } else {
                Vec3::ZERO
            };
            ctx.scene.physics_mut().set_gravity(gravity);
            // 失重时所有刚体都得叫醒，睡着的不会自己浮起来。
            klog::info!("重力{}", if self.gravity_on { "已恢复" } else { "已关闭" });
        }

        if ctx.input.action_just_pressed("ragdoll") {
            if self.ragdoll.is_none() {
                self.build_soldier_ragdoll(ctx);
            }
            if let Some(ragdoll) = ctx
                .scene
                .try_get_mut(self.ragdoll)
                .and_then(Node::ragdoll_mut)
            {
                let on = !ragdoll.is_active();
                ragdoll.set_active(on);
                klog::info!("布娃娃{}", if on { "已激活（物理接管）" } else { "已关闭（动画接管）" });
            }
        }

        // 触发区：把碰撞事件解回节点，报告谁进来了。
        let trigger = self.trigger;
        let mut entered = Vec::new();
        for event in ctx.scene.collision_events() {
            if !event.started || !event.sensor {
                continue;
            }
            let (a, b) = ctx.scene.collision_nodes(event);
            let other = if a == Some(trigger) { b } else { a };
            if a == Some(trigger) || b == Some(trigger) {
                entered.push(other);
            }
        }
        for node in entered {
            let name = node
                .and_then(|h| ctx.scene.try_get(h))
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "(已销毁)".to_string());
            klog::info!("触发区：「{name}」进来了");
        }
    }


    // ───────────────────────── 场景存读 ─────────────────────────

    /// 存盘路径。放在临时目录，免得往仓库里拉屎。
    fn scene_path() -> std::path::PathBuf {
        std::env::temp_dir().join("kengine_demo_scene.bin")
    }

    /// 把当前场景写进文件。
    fn save_scene(ctx: &mut Context) {
        let path = Self::scene_path();
        let before = ctx.stats;

        match ctx.scene.save(&path) {
            Ok(()) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                klog::info!(
                    "场景已存盘：{}（{:.1} KB）；存盘时画面为 {} 绘制 / {} 三角形",
                    path.display(),
                    size as f64 / 1024.0,
                    before.drawn,
                    before.triangles,
                );
            }
            Err(error) => klog::error!("存盘失败：{error:?}"),
        }
    }

    /// `--roundtrip` 的自检流程：跑几秒 → 存盘 → 读档 → 对比统计。
    ///
    /// 存在的理由是「画面与状态完全一致」这条验收标准没法靠肉眼确认，
    /// 而按键又没法在无头环境里按。
    fn drive_roundtrip(&mut self, ctx: &mut Context) {
        if !self.roundtrip {
            return;
        }
        match (self.roundtrip_step, ctx.elapsed) {
            // 先等资源加载完、场景稳定下来。
            (0, t) if t > 3.0 => {
                self.stats_before = Some(ctx.stats);
                Self::save_scene(ctx);
                self.roundtrip_step = 1;
            }
            (1, _) => {
                self.load_scene(ctx);
                self.roundtrip_step = 2;
            }
            // 读回来之后隔一帧再看统计：剔除与批处理要重新跑一轮。
            (2, t) if t > 3.5 => {
                let before = self.stats_before.unwrap_or_default();
                let after = ctx.stats;
                klog::info!(
                    "自检对比 —— 存盘前：{} 绘制 / {} 三角形 / {} 绘制调用 / {} 粒子",
                    before.drawn,
                    before.triangles,
                    before.draw_calls,
                    before.particles,
                );
                klog::info!(
                    "自检对比 —— 读档后：{} 绘制 / {} 三角形 / {} 绘制调用 / {} 粒子",
                    after.drawn,
                    after.triangles,
                    after.draw_calls,
                    after.particles,
                );
                if before.drawn == after.drawn && before.triangles == after.triangles {
                    klog::info!("自检通过：几何完全一致（粒子不在存档范围内，归零是预期的）");
                } else {
                    klog::error!("自检未通过：读回来的画面和存盘前不一样");
                }
                self.roundtrip_step = 3;
                ctx.request_exit();
            }
            _ => {}
        }
    }

    /// 从文件读回场景，整个替换掉当前场景。
    ///
    /// 节点句柄在存读之间是**稳定**的（句柄的世代号一并存了下来），
    /// 所以 `self.cube` 这些记着的句柄读回来依然指向同一个节点。
    fn load_scene(&mut self, ctx: &mut Context) {
        let path = Self::scene_path();
        if !path.exists() {
            klog::warn!("还没有存过盘，先按 F5");
            return;
        }

        match Scene::load(&path, Some(ctx.resources)) {
            Ok(scene) => {
                *ctx.scene = scene;
                // 粒子、动画播放器、布娃娃不参与序列化，读回来这些组件就没了。
                // 只清掉指向它们的本地状态，**不要**把 `*_spawned` 复位——
                // 模型的几何已经在存档里了，再实例化一遍就成了两份。
                self.lion_morphs.clear();
                self.locomotion = None;
                self.ragdoll = Handle::NONE;
                klog::info!(
                    "场景已读回：{} 个节点；粒子/动画/布娃娃不在存档范围内，已丢弃",
                    ctx.scene.nodes().alive_count()
                );
            }
            Err(error) => klog::error!("读档失败：{error:?}"),
        }
    }


    // ───────────────────────── 2D 精灵 ─────────────────────────

    /// 程序化生成一张 4×2 格的精灵表，每格一个纯色。
    ///
    /// 每格颜色不同，动画一跑就能直接看出「取到的是哪一格」——
    /// 用一张真实的角色表反而不容易一眼确认 UV 有没有错位。
    fn sprite_sheet() -> Texture {
        const COLUMNS: u32 = 4;
        const ROWS: u32 = 2;
        const CELL: u32 = 32;
        const COLORS: [[u8; 4]; 8] = [
            [235, 64, 52, 255],   // 红
            [235, 158, 52, 255],  // 橙
            [235, 232, 52, 255],  // 黄
            [106, 235, 52, 255],  // 绿
            [52, 235, 213, 255],  // 青
            [52, 116, 235, 255],  // 蓝
            [147, 52, 235, 255],  // 紫
            [235, 52, 177, 255],  // 品红
        ];

        let (width, height) = (COLUMNS * CELL, ROWS * CELL);
        let mut data = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let cell = (y / CELL) * COLUMNS + (x / CELL);
                let color = COLORS[cell as usize % COLORS.len()];
                // 每格留一圈深色边，格子的边界一眼可见。
                let (lx, ly) = (x % CELL, y % CELL);
                let on_border = lx < 2 || ly < 2 || lx >= CELL - 2 || ly >= CELL - 2;
                let pixel = if on_border { [20, 20, 24, 255] } else { color };

                let offset = ((y * width + x) * 4) as usize;
                data[offset..offset + 4].copy_from_slice(&pixel);
            }
        }

        Texture::new(width, height, data).with_sampler(Sampler::pixelated())
    }

    /// 放一排精灵：静止的一格、一条循环动画、一条来回播的动画。
    fn spawn_sprites(&mut self, ctx: &mut Context) {
        let sheet = ctx.resources.register("builtin/sprite_sheet", Self::sprite_sheet());
        let atlas = Atlas::grid(4, 2);

        // 静止：直接取第 0 行第 2 格。
        let still = Sprite::from_region(atlas.region(2).unwrap())
            .with_size(Vec2::splat(0.8))
            .with_anchor(Anchor::BottomCenter);
        self.spawn_sprite(ctx, "SpriteStill", &still, &sheet, Vec3::new(-2.0, -1.0, 3.0));

        // 循环：第 0 行的四格。
        self.sprite_loop = SpriteAnimation::new(atlas.row(0), 6.0);
        self.sprite_loop_node = self.spawn_sprite(
            ctx,
            "SpriteLoop",
            &still.with_region(self.sprite_loop.frame()),
            &sheet,
            Vec3::new(-1.0, -1.0, 3.0),
        );

        // 来回播：第 1 行的四格。两端各只出现一次，接缝比循环连贯。
        self.sprite_ping = SpriteAnimation::new(atlas.row(1), 6.0).with_mode(PlayMode::PingPong);
        self.sprite_ping_node = self.spawn_sprite(
            ctx,
            "SpritePingPong",
            &still.with_region(self.sprite_ping.frame()),
            &sheet,
            Vec3::new(0.0, -1.0, 3.0),
        );
    }

    /// 建一个精灵节点：方片网格 + 带图集 UV 变换的材质。
    fn spawn_sprite(
        &mut self,
        ctx: &mut Context,
        name: &str,
        sprite: &Sprite,
        sheet: &Resource<Texture>,
        position: Vec3,
    ) -> Handle<Node> {
        ctx.scene.add_node(
            Node::new(name)
                .with_mesh(sprite.quad())
                .with_material(Self::sprite_material(sprite, sheet))
                .with_position(position),
        )
    }

    /// 精灵材质：贴图 + 图集 UV 变换，并且**不受光照影响**。
    ///
    /// 2D 精灵按惯例是「画上去什么样就是什么样」，走 PBR 会被场景里的
    /// 方向光和环境光染色。这里把基础色压黑、全部亮度放进自发光，
    /// 于是它在现有管线里表现为无光照的贴图——不必为此另开一条渲染路径。
    fn sprite_material(sprite: &Sprite, sheet: &Resource<Texture>) -> Material {
        Material::standard()
            .with_base_color(Vec4::new(0.0, 0.0, 0.0, 1.0))
            .with_base_color_texture(sheet.clone())
            .with(kengine::kpbr::standard::EMISSIVE, Vec3::ONE)
            .with(kengine::kpbr::standard::EMISSIVE_TEXTURE, sheet.clone())
            .with(kengine::kpbr::standard::UV_SCALE, sprite.uv_scale())
            .with(kengine::kpbr::standard::UV_OFFSET, sprite.uv_offset())
    }

    /// 每帧推进两条精灵动画，并把当前帧写进材质。
    ///
    /// 换帧只改两个数值参数，网格一动不动——这正是把 UV 变换放进材质
    /// 而不是烘进顶点的理由。
    fn drive_sprites(&mut self, ctx: &mut Context) {
        self.sprite_loop.tick(ctx.dt);
        self.sprite_ping.tick(ctx.dt);

        for (handle, region) in [
            (self.sprite_loop_node, self.sprite_loop.frame()),
            (self.sprite_ping_node, self.sprite_ping.frame()),
        ] {
            let Some(material) = ctx
                .scene
                .try_get_mut(handle)
                .and_then(Node::material_mut)
            else {
                continue;
            };
            material.set(kengine::kpbr::standard::UV_SCALE, region.uv_scale());
            material.set(kengine::kpbr::standard::UV_OFFSET, region.uv_offset());
        }
    }


    // ───────────────────────── 音频 ─────────────────────────

    /// 程序化生成一段可辨认的音效，省得往仓库里塞二进制素材。
    ///
    /// 两个八度关系的正弦叠一层，再套一个指数衰减包络——比纯正弦好认，
    /// 也更像一个「音效」而不是测试信号。
    fn beep(frequency: f32, seconds: f32) -> AudioBuffer {
        const RATE: u32 = 48_000;
        let frames = (seconds * RATE as f32) as usize;
        let samples = (0..frames)
            .map(|index| {
                let t = index as f32 / RATE as f32;
                let envelope = (-4.0 * t / seconds.max(1e-3)).exp();
                let base = (std::f32::consts::TAU * frequency * t).sin();
                let octave = (std::f32::consts::TAU * frequency * 2.0 * t).sin() * 0.3;
                (base + octave) * envelope * 0.4
            })
            .collect();

        AudioBuffer::new(samples, 1, RATE)
    }

    /// 一段能循环得上的低频嗡鸣，给绕圈的声源当音色。
    fn hum(frequency: f32) -> AudioBuffer {
        const RATE: u32 = 48_000;
        // 帧数取成整周期数，首尾才接得上——差一点点就会每循环一次「哒」一声。
        let periods = 20.0;
        let frames = (periods * RATE as f32 / frequency).round() as usize;
        let samples = (0..frames)
            .map(|index| {
                let phase = std::f32::consts::TAU * frequency * index as f32 / RATE as f32;
                (phase.sin() * 0.5 + (phase * 3.0).sin() * 0.15) * 0.35
            })
            .collect();

        AudioBuffer::new(samples, 1, RATE)
    }

    /// 放一个绕着场景转圈的 3D 声源，外加一段 2D 背景音。
    fn spawn_audio(&mut self, ctx: &mut Context) {
        ctx.resources.add_loader(AudioLoader);

        // 程序化生成的音频直接登记为资源，不需要外部文件。
        let hum = ctx.resources.register("builtin/hum", Self::hum(110.0));
        self.beep_sound = Some(ctx.resources.register("builtin/beep", Self::beep(660.0, 0.35)));

        // 绕圈的 3D 声源：挂个小球好看出它在哪。
        self.audio_orbit = ctx.scene.add_node(
            Node::new("AudioEmitter")
                .with_mesh(Mesh::sphere(12, 16))
                .with_material(PbrMaterial::emissive(
                    Vec3::new(0.2, 0.9, 0.4),
                    Vec3::new(0.3, 2.0, 0.8),
                ))
                .with_scale(Vec3::splat(0.25))
                .with_sound(
                    SoundSource::spatial(
                        hum,
                        // 反比衰减、5 米参考距离：走近明显变响，走远迅速淡出。
                        Spatial::default()
                            .with_range(5.0, 60.0)
                            .with_model(Attenuation::Inverse, 1.2),
                    )
                    .looping()
                    .with_gain(0.8),
                ),
        );

        klog::info!(
            "音频：{}；绿色小球是 3D 声源，绕圈时能听出左右与远近，B 键放一声提示音",
            match ctx.audio.name() {
                Some(name) => format!("输出到「{name}」"),
                None => "没有可用输出，静默运行".to_string(),
            }
        );
    }

    /// 每帧驱动音频：让 3D 声源绕圈，处理按键。
    fn drive_audio(&mut self, ctx: &mut Context) {
        if self.audio_orbit.is_some() {
            let angle = ctx.elapsed * 0.6;
            let radius = 6.0;
            ctx.scene[self.audio_orbit].transform.position =
                Vec3::new(angle.cos() * radius, 0.5, angle.sin() * radius);
        }

        // 一次性音效：每按一次新建一个节点，播完由引擎自己回收。
        if ctx.input.action_just_pressed("beep")
            && let Some(beep) = self.beep_sound.clone()
        {
            let node = ctx.scene.add_node(
                Node::new("Beep")
                    .with_position(Vec3::new(0.0, 1.0, 0.0))
                    .with_sound(SoundSource::new(beep).with_gain(0.7)),
            );
            self.beep_nodes.push(node);
            klog::info!("嘀");
        }

        // 播完的一次性音效连节点一起清掉，免得越积越多。
        self.beep_nodes.retain(|handle| {
            let finished = ctx
                .scene
                .try_get(*handle)
                .and_then(Node::sound)
                .is_some_and(SoundSource::is_finished);
            if finished {
                ctx.scene.remove_node(*handle);
            }
            !finished
        });

        if ctx.input.action_just_pressed("mute") {
            let mut mixer = ctx.audio.mixer().lock();
            mixer.master_gain = if mixer.master_gain > 0.0 { 0.0 } else { 1.0 };
            let muted = mixer.master_gain == 0.0;
            drop(mixer);
            klog::info!("音频{}", if muted { "已静音" } else { "已恢复" });
        }
    }


    // ───────────────────────── 脚本 ─────────────────────────

    /// 一个自转 + 上下浮动的方块。
    ///
    /// 演示闭包状态（`elapsed` 每实例一份）、`engine.self()`、按 dt 积分、
    /// 以及往 Rust 侧抛事件。
    const SPINNER_JS: &str = r#"
let elapsed = 0;
let direction = 1;
let reports = 0;

return {
    init() {
        engine.log("spinner 启动，挂在「" + engine.name(engine.self()) + "」上");
    },

    update(dt) {
        elapsed += dt;
        const me = engine.self();
        engine.rotateY(me, dt * 1.5);

        // 读自己的位置，算出新的，写回去。
        const p = engine.position(me);
        if (p.y > 1.6) direction = -1;
        if (p.y < 0.4) direction = 1;
        engine.setPosition(me, p.x, p.y + direction * dt * 0.8, p.z);

        // 每两秒给 Rust 侧抛一次事件。
        if (elapsed > 2 * (reports + 1)) {
            reports += 1;
            engine.emit("spinner.tick", reports);
        }
    },

    destroy() {
        engine.log("spinner 收工，共跑了 " + elapsed.toFixed(1) + " 秒");
    },
};
"#;

    /// 绕着 spinner 转圈的小方块，演示跨节点访问。
    ///
    /// 它自己不记位置——每帧读目标的**世界**坐标再算偏移，
    /// 所以目标怎么动它都跟得上。
    const FOLLOWER_JS: &str = r#"
let angle = 0;

return {
    update(dt) {
        angle += dt * 2.0;

        const target = engine.find("ScriptSpinner");
        if (target === 4294967295) {
            // 目标没了（被删了），安静待着就行。
            return;
        }

        const t = engine.worldPosition(target);
        engine.setPosition(
            engine.self(),
            t.x + Math.cos(angle) * 1.2,
            t.y,
            t.z + Math.sin(angle) * 1.2,
        );
    },
};
"#;

    /// 放两个被脚本驱动的方块。
    ///
    /// 源码直接内嵌并登记为资源，与内置贴图、程序化音频同一个路子——
    /// demo 不该往仓库里塞外部文件。真实项目里 `.js` 走 [`ScriptLoader`]
    /// 从磁盘加载，那条路径由 kscript / kscene 的测试覆盖。
    fn spawn_scripts(&mut self, ctx: &mut Context) {
        ctx.resources.add_loader(ScriptLoader);

        let spinner = ctx
            .resources
            .register("builtin/spinner.js", Script::new(Self::SPINNER_JS, "spinner.js"));
        let follower = ctx
            .resources
            .register("builtin/follower.js", Script::new(Self::FOLLOWER_JS, "follower.js"));

        self.script_spinner = ctx.scene.add_node(
            Node::new("ScriptSpinner")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::new(0.9, 0.6, 0.2), 0.3))
                .with_position(Vec3::new(-6.0, 1.0, 0.0))
                .with_scale(Vec3::splat(0.6))
                .with_script(ScriptComponent::new(spinner)),
        );

        ctx.scene.add_node(
            Node::new("ScriptFollower")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::emissive(
                    Vec3::new(0.3, 0.5, 1.0),
                    Vec3::new(0.4, 0.8, 2.5),
                ))
                .with_scale(Vec3::splat(0.25))
                .with_script(ScriptComponent::new(follower)),
        );

        klog::info!("脚本：橙色方块由 JS 驱动自转+浮动，蓝色小块绕着它转；J 键停掉脚本");
    }

    /// 处理脚本事件，并响应按键。
    fn drive_scripts(&mut self, ctx: &mut Context) {
        // 脚本排在插件 update 之前跑，所以这里读到的是**本帧**的事件。
        for event in ctx.script_events {
            klog::info!(
                "收到脚本事件：{} = {}（来自节点 {:?}）",
                event.name,
                event.value,
                event.source
            );
        }

        if ctx.input.action_just_pressed("script") {
            let Some(component) = ctx
                .scene
                .try_get_mut(self.script_spinner)
                .and_then(Node::script_mut)
            else {
                return;
            };
            component.enabled = !component.enabled;
            let enabled = component.enabled;
            klog::info!("脚本{}", if enabled { "已恢复" } else { "已停用" });
        }
    }

    fn report(ctx: &Context) {
        let stats = ctx.stats;
        {
            let mixer = ctx.audio.mixer().lock();
            klog::info!(
                "音频：{} 个声源（{} 在播）；累计 {} 帧，上一块峰值 {:.3}{}",
                mixer.len(),
                mixer.playing_count(),
                mixer.rendered_frames(),
                mixer.last_peak(),
                if ctx.audio.is_silent() { "（静默模式）" } else { "" },
            );
        }
        let physics = ctx.scene.physics().stats();
        klog::info!(
            "物理：刚体 {} / 碰撞体 {} / 关节 {}；单步 {} µs",
            physics.body_count,
            physics.collider_count,
            ctx.scene.physics().joint_count(),
            physics.step_time.as_micros(),
        );
        klog::info!(
            "绘制 {} / 剔除 {} / 共 {}；三角形 {}；粒子 {}；\
             绘制调用 {}（平均 {:.1} 个/次）；剔除 {} µs，CPU 准备 {} µs",
            stats.drawn,
            stats.culled,
            stats.total(),
            stats.triangles,
            stats.particles,
            stats.draw_calls,
            stats.instances_per_draw(),
            stats.cull_micros,
            stats.prepare_micros,
        );
    }
}

impl Plugin for Game {
    fn init(&mut self, ctx: &mut Context) {
        // ── 输入映射：逻辑里只认动作名，不认具体按键 ──
        let bindings = ctx.input.bindings_mut();
        bindings.bind_action("pause", KeyCode::Space);
        bindings.bind_action("quit", KeyCode::Escape);
        bindings.bind_action("reset", KeyCode::KeyR);
        bindings.bind_action("stats", KeyCode::KeyC);
        bindings.bind_action("burst", KeyCode::KeyF);
        bindings.bind_action("stand", KeyCode::Digit1);
        bindings.bind_action("walk", KeyCode::Digit2);
        bindings.bind_action("run", KeyCode::Digit3);
        bindings.bind_action("morph", KeyCode::KeyM);
        bindings.bind_action("shoot", KeyCode::KeyP);
        bindings.bind_action("restack", KeyCode::KeyX);
        bindings.bind_action("gravity", KeyCode::KeyG);
        bindings.bind_action("ragdoll", KeyCode::KeyK);
        bindings.bind_action("save", KeyCode::F5);
        bindings.bind_action("load", KeyCode::F9);
        bindings.bind_action("beep", KeyCode::KeyB);
        bindings.bind_action("mute", KeyCode::KeyN);
        bindings.bind_action("script", KeyCode::KeyJ);
        bindings.bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
        bindings.bind_axis("forward", KeyCode::KeyW, KeyCode::KeyS);
        bindings.bind_axis("vertical", KeyCode::KeyE, KeyCode::KeyQ);

        // ── 资源：注册加载器并异步请求配置 ──
        ctx.resources.add_loader(LevelConfigLoader);
        ctx.resources.add_loader(TextureLoader);
        ctx.resources.add_loader(ShaderLoader);
        ctx.resources.add_loader(GltfLoader);
        self.config = Some(ctx.resources.request("assets/demo.level"));
        self.model = Some(ctx.resources.request("assets/gem.glb"));
        self.soldier = Some(ctx.resources.request("assets/Soldier.glb"));
        self.lion = Some(ctx.resources.request("assets/lion.glb"));

        // 程序化生成的棋盘格贴图，直接登记为资源，无需外部图片文件。
        let checker = ctx.resources.register(
            "builtin/checker",
            Texture::checkerboard(64, 8, [230, 230, 235, 255], [40, 44, 60, 255])
                .with_sampler(Sampler::pixelated()),
        );

        // ── 光照：方向光当主光，点光源与聚光灯做点缀 ──
        // 光源是普通场景节点，朝向取节点的 -Z 轴。
        ctx.scene.add_node(
            Node::new("SunLight")
                .with_light(Light::directional().with_intensity(2.5).with_shadows())
                .with_transform(Transform::looking_at(
                    Vec3::new(4.0, 6.0, 4.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        self.lamp = ctx.scene.add_node(
            Node::new("Lamp").with_light(
                Light::point(9.0)
                    .with_color(Vec3::new(1.0, 0.35, 0.1))
                    .with_intensity(40.0),
            ),
        );

        ctx.scene.add_node(
            Node::new("Spot")
                .with_light(
                    Light::spot(18.0, 12.0, 26.0)
                        .with_color(Vec3::new(0.4, 0.7, 1.0))
                        .with_intensity(60.0),
                )
                .with_transform(Transform::looking_at(
                    Vec3::new(-4.0, 5.0, 2.0),
                    Vec3::new(-2.0, 0.0, 0.0),
                    Vec3::Y,
                )),
        );

        // ── 场景 ──
        ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 2.6, 8.0),
                    Vec3::new(0.0, 1.6, 0.0),
                    Vec3::Y,
                )),
        );

        // 程序化法线贴图，用来验证切线空间是否正确。
        let bumps = ctx.resources.register(
            "builtin/bumps",
            Texture::bumpy_normal(128, 4),
        );

        // 贴了棋盘格的立方体，金属度高、粗糙度低 → 高光锐利。
        // 挂上法线贴图后表面会呈现凹凸感，而几何体本身仍是平的。
        self.cube = ctx.scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(
                    Material::standard()
                        .with_base_color_texture(checker.clone())
                        .with(kengine::kpbr::standard::NORMAL_TEXTURE, bumps)
                        .with_metallic(0.9)
                        .with_roughness(0.25),
                ),
        );

        // 子节点跟随父节点一起转，体现场景图层级。橙色、无贴图。
        self.orbit = ctx.scene.add_node_with_parent(
            Node::new("Orbit")
                .with_mesh(Mesh::cube())
                .with_material(
                    // 自发光：不受光照影响，会成为 Bloom 的来源。
                    PbrMaterial::emissive(
                        Vec3::new(1.0, 0.45, 0.1),
                        Vec3::new(3.0, 1.2, 0.2),
                    ),
                )
                .with_position(Vec3::new(1.5, 0.0, 0.0))
                .with_scale(Vec3::splat(0.3)),
            self.cube,
        );

        // 地面平铺同一张贴图——共享资源不会重复上传显存。
        ctx.scene.add_node(
            Node::new("Ground")
                .with_mesh(Mesh::plane(8.0))
                .with_material(
                    Material::standard()
                        .with_base_color_texture(checker)
                        .with_base_color(Vec4::new(0.6, 0.6, 0.7, 1.0))
                        .with_roughness(0.9),
                )
                .with_position(Vec3::new(0.0, -1.0, 0.0))
                .with_scale(Vec3::splat(8.0)),
        );

        // PBR 参数网格：横向粗糙度递增，纵向从电介质到金属。
        // 这是检验 PBR 是否正确最直观的排布。
        let sphere = Mesh::sphere(24, 32);
        const COLUMNS: usize = 7;
        const ROWS: usize = 2;
        for row in 0..ROWS {
            let metallic = row as f32 / (ROWS - 1) as f32;
            for column in 0..COLUMNS {
                // 粗糙度不取到 0，完全光滑的表面在只有一盏方向光时几乎全黑。
                let roughness = 0.05 + column as f32 / (COLUMNS - 1) as f32 * 0.95;
                ctx.scene.add_node(
                    Node::new(format!("Pbr{row}_{column}"))
                        .with_mesh(sphere.clone())
                        .with_material(if metallic > 0.5 {
                            PbrMaterial::metal(Vec3::new(1.0, 0.766, 0.336), roughness)
                        } else {
                            PbrMaterial::dielectric(Vec3::new(0.24, 0.42, 0.85), roughness)
                        })
                        .with_position(Vec3::new(
                            (column as f32 - (COLUMNS - 1) as f32 / 2.0) * 1.3,
                            2.4 + row as f32 * 1.3,
                            -3.0,
                        ))
                        .with_scale(Vec3::splat(1.1)),
                );
            }
        }

        // 一圈球体，其中大半在视野外——用来观察视锥剔除是否真的生效。
        let ring_mesh = Mesh::sphere(12, 18);
        for index in 0..48 {
            let angle = index as f32 / 48.0 * std::f32::consts::TAU;
            let radius = 14.0;
            ctx.scene.add_node(
                Node::new(format!("Ring{index}"))
                    .with_mesh(ring_mesh.clone())
                    .with_material(PbrMaterial::dielectric(Vec3::new(0.4, 0.9, 0.5), 0.4))
                    .with_position(Vec3::new(
                        angle.cos() * radius,
                        -0.4,
                        angle.sin() * radius,
                    )),
            );
        }

        // ── 粒子：两种混合方式各来一个 ──
        // 相加混合的火花：亮部会溢出到 Bloom 里。
        self.sparks = ctx.scene.add_node(
            Node::new("Sparks")
                .with_particles(
                    ParticleSystem::new(
                        Emitter::cone(18.0)
                            .with_rate(220.0)
                            .with_speed((2.5, 4.5))
                            .with_lifetime((0.6, 1.2))
                            .with_size((0.05, 0.12)),
                    )
                    .with_acceleration(Vec3::new(0.0, -6.0, 0.0))
                    .with_blend(BlendMode::Additive)
                    .with_color(ColorGradient::new(
                        [
                            (0.0, Vec4::new(4.0, 2.4, 0.6, 1.0)),
                            (0.35, Vec4::new(3.0, 0.7, 0.1, 1.0)),
                            (1.0, Vec4::new(0.6, 0.05, 0.0, 0.0)),
                        ],
                        Vec4::ONE,
                    ))
                    .with_size_curve(Curve::linear(1.0, 0.35))
                    // 火花与场景真实几何碰撞：会从地面和箱子上弹起来。
                    // 每次弹跳都折掉三成寿命，弹几下就熄灭，比一直弹到寿终自然。
                    .with_collision(
                        Collision::scene().with_response(
                            CollisionResponse::bouncy()
                                .with_material(0.45, 0.3)
                                .with_lifetime_loss(0.3),
                        ),
                    )
                    // 世界空间：粒子出生后就与发射器节点脱钩，
                    // 否则碰撞算出来的世界坐标会被节点变换再乘一遍。
                    .with_space(Space::World)
                    .with_seed(0xF12E),
                )
                .with_position(Vec3::new(3.2, -0.6, 1.0)),
        );

        // 半透明烟雾：贴着地面缓缓上升、逐渐变大变淡。
        ctx.scene.add_node(
            Node::new("Smoke")
                .with_particles(
                    ParticleSystem::new(
                        Emitter::disk(0.6)
                            .with_rate(28.0)
                            .with_speed((0.3, 0.7))
                            .with_spread_degrees(25.0)
                            .with_lifetime((2.5, 4.0))
                            .with_size((0.5, 0.9))
                            .with_rotation_speed((-0.6, 0.6)),
                    )
                    .with_acceleration(Vec3::new(0.25, 0.15, 0.0))
                    .with_damping(0.6)
                    .with_color(ColorGradient::fade_in_out(Vec3::splat(0.35), 0.25))
                    .with_size_curve(Curve::linear(0.6, 2.2))
                    // 烟雾只需要不穿地板，一块平面就够——
                    // 为它做逐粒子射线检测是纯浪费。
                    .with_collision(
                        Collision::ground(-1.0).with_response(CollisionResponse::sticky()),
                    )
                    .with_space(Space::World)
                    .with_seed(20260817),
                )
                .with_position(Vec3::new(-3.4, -0.9, 1.2)),
        );

        self.spawn_physics_playground(ctx);
        self.spawn_sprites(ctx);
        self.spawn_audio(ctx);
        self.spawn_scripts(ctx);
        self.spawn_stress_field(ctx);

        klog::info!(
            "WASD 移动，Q/E 升降，空格暂停，R 重置，C 打印统计，F 喷火花，\
             1/2/3 切换士兵动作，M 开关狮子形变，Esc 退出"
        );
        klog::info!("物理：P 开火（射线拾取 + 冲量），X 重码箱子，G 开关重力，K 切换士兵布娃娃");
        klog::info!("资源：F5 存盘，F9 读档；音频：B 放提示音，N 静音；脚本：J 开关");
    }

    fn update(&mut self, ctx: &mut Context) {
        // 骨骼动画模型同样是异步加载的，就绪后建状态机。
        if !self.soldier_spawned {
            self.spawn_soldier(ctx);
        }
        self.drive_soldier(ctx);

        // 形变目标模型同样是异步加载的。
        if !self.lion_spawned {
            self.spawn_lion(ctx);
        }
        self.drive_lion(ctx);

        self.drive_physics(ctx);
        self.drive_sprites(ctx);
        self.drive_audio(ctx);
        self.drive_scripts(ctx);
        self.drive_roundtrip(ctx);

        if ctx.input.action_just_pressed("morph") {
            self.lion_talking = !self.lion_talking;
            klog::info!("狮子形变{}", if self.lion_talking { "已开启" } else { "已停止" });
        }

        if ctx.input.action_just_pressed("stand") {
            self.target_speed = 0.0;
        }
        if ctx.input.action_just_pressed("walk") {
            self.target_speed = 1.0;
        }
        if ctx.input.action_just_pressed("run") {
            self.target_speed = 4.0;
        }

        // glTF 模型在后台加载，就绪的那一帧实例化进场景。
        if !self.model_spawned
            && let Some(handle) = &self.model
            && let Some(model) = handle.data_ref()
        {
            let root = ctx.scene.root();
            let instance = ctx.scene.instantiate_model(&model, root);
            ctx.scene[instance].transform.position = Vec3::new(-2.0, 0.2, 0.0);
            self.model_spawned = true;
            klog::info!(
                "模型已载入：{} 个三角形，{} 种材质",
                model.triangle_count(),
                model.materials().len()
            );
        }

        // 启动 2 秒后自动汇报一次渲染统计，方便无人值守时确认剔除生效。
        if !self.stats_reported && ctx.elapsed > self.report_at.max(2.0) {
            self.stats_reported = true;
            Self::report(ctx);
        }

        // 配置是异步加载的，没就绪时用默认值，就绪后自动生效。
        let (spin_speed, move_speed) = self
            .config
            .as_ref()
            .and_then(|c| c.data_ref().map(|d| (d.spin_speed, d.move_speed)))
            .unwrap_or((1.0, 2.0));

        if ctx.input.action_just_pressed("quit") {
            ctx.request_exit();
            return;
        }

        if ctx.input.action_just_pressed("pause") {
            self.paused = !self.paused;
            klog::info!("旋转{}", if self.paused { "已暂停" } else { "已恢复" });
        }

        if ctx.input.action_just_pressed("stats") {
            Self::report(ctx);
        }

        // 一次性喷发：爆炸、受击这类效果都是这么做的，
        // 与按速率持续生成互不影响。
        if ctx.input.action_just_pressed("burst") {
            let world = ctx.scene[self.sparks].global_transform();
            if let Some(system) = ctx.scene[self.sparks].particles_mut() {
                system.burst(240, world);
            }
        }

        if ctx.input.action_just_pressed("reset") {
            ctx.scene[self.cube].transform = Transform::IDENTITY;
            klog::info!("已重置");
        }

        // 移动：读取语义化的轴，而不是具体按键。
        let plane = ctx.input.axis_vector("horizontal", "forward");
        let lift = ctx.input.axis("vertical");
        let velocity = Vec3::new(plane.x, lift, -plane.y) * move_speed * ctx.dt;
        ctx.scene[self.cube].transform.translate(velocity);

        // 点光源绕圈移动，能直观看出距离衰减与多光源叠加。
        let orbit = ctx.elapsed * 0.7;
        ctx.scene[self.lamp].transform.position =
            Vec3::new(orbit.cos() * 3.2, 1.1, orbit.sin() * 3.2);

        if !self.paused {
            ctx.scene[self.cube].transform.rotate_y(spin_speed * ctx.dt);
            ctx.scene[self.orbit].transform.rotate_x(spin_speed * 2.0 * ctx.dt);
        }
    }
}

/// `--pack`：把 `assets/` 打成一个资源包，并让引擎从包里读。
///
/// 这演示的是发布形态：玩家看到的是一个 `.kpak`，而不是一地散文件。
/// 分层放在包前面是为了开发方便——散文件还在的话优先用散文件，改完立刻生效。
fn packed_io() -> Option<std::sync::Arc<dyn kengine::kasset::ResourceIo>> {
    use kengine::kasset::{FsResourceIo, LayeredResourceIo, PackResourceIo, PackWriter};
    use std::sync::Arc;

    if !std::env::args().any(|arg| arg == "--pack") {
        return None;
    }

    let mut writer = PackWriter::new();
    let count = writer.add_directory("assets").unwrap_or(0);
    let bytes = writer.finish();
    klog::info!(
        "已把 assets/ 打包：{} 个文件，{:.1} KB",
        count,
        bytes.len() as f64 / 1024.0
    );

    let pack = PackResourceIo::from_vec(bytes).expect("刚打出来的包应当能读");
    Some(Arc::new(
        LayeredResourceIo::new()
            .with(Arc::new(FsResourceIo))
            .with(Arc::new(pack)),
    ))
}

/// 解析 `--stress N`，没给就返回 0。
fn stress_count() -> usize {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.split_once('=') {
            Some(("--stress", value)) => Some(value.to_string()),
            _ if arg == "--stress" => args.next(),
            _ => None,
        };
        if let Some(value) = value {
            return value.parse().unwrap_or(0);
        }
    }
    0
}

fn main() {
    klog::init(None);

    // 除了完整插件，也可以往指定阶段直接挂一段逻辑。
    // 这里演示在帧末统计平均帧率——物理、动画这类系统同样按这种方式接入。
    // 剔除与准备耗时取区间平均：单帧采样受调度抖动影响太大，说明不了问题。
    let mut frames = 0u32;
    let mut window_frames = 0u32;
    let mut cull_micros = 0u64;
    let mut prepare_micros = 0u64;
    let mut next_report = 5.0;

    let mut app = App::new().with_title("kengine demo");
    if let Some(io) = packed_io() {
        app = app.with_resource_io(io);
    }

    app
        .add_plugin(Game {
            stress: stress_count(),
            roundtrip: std::env::args().any(|arg| arg == "--roundtrip"),
            // 重力默认开着，`Default` 给的 false 与实际状态相反。
            gravity_on: true,
            ..Game::default()
        })
        .add_system(Stage::FrameEnd, move |ctx| {
            frames += 1;
            window_frames += 1;
            cull_micros += ctx.stats.cull_micros as u64;
            prepare_micros += ctx.stats.prepare_micros as u64;

            if ctx.elapsed >= next_report {
                klog::info!(
                    "平均帧率：{:.0} FPS；剔除 {:.0} µs/帧，CPU 准备 {:.0} µs/帧",
                    frames as f32 / ctx.elapsed,
                    cull_micros as f64 / window_frames as f64,
                    prepare_micros as f64 / window_frames as f64,
                );
                window_frames = 0;
                cull_micros = 0;
                prepare_micros = 0;
                next_report += 5.0;
            }
        })
        .run();
}
