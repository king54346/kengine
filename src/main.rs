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
    paused: bool,
}

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
    fn report(ctx: &Context) {
        let stats = ctx.stats;
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
                    .with_seed(20260817),
                )
                .with_position(Vec3::new(-3.4, -0.9, 1.2)),
        );

        self.spawn_stress_field(ctx);

        klog::info!(
            "WASD 移动，Q/E 升降，空格暂停，R 重置，C 打印统计，F 喷火花，\
             1/2/3 切换士兵动作，M 开关狮子形变，Esc 退出"
        );
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

    App::new()
        .with_title("kengine demo")
        .add_plugin(Game {
            stress: stress_count(),
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
