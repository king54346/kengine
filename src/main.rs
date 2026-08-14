//! 用 kengine 写的示例游戏。
//!
//! 引擎负责窗口、事件循环、输入采集和渲染；这个文件里只有游戏逻辑。
//!
//! 操作：WASD 移动方块，Q/E 升降，空格暂停自转，R 重置，Esc 退出。

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
    cube: Handle<Node>,
    orbit: Handle<Node>,
    config: Option<Resource<LevelConfig>>,
    /// 异步加载的 glTF 模型，就绪后再实例化进场景。
    model: Option<Resource<Model>>,
    model_spawned: bool,
    stats_reported: bool,
    paused: bool,
}

impl Plugin for Game {
    fn init(&mut self, ctx: &mut Context) {
        // ── 输入映射：逻辑里只认动作名，不认具体按键 ──
        let bindings = ctx.input.bindings_mut();
        bindings.bind_action("pause", KeyCode::Space);
        bindings.bind_action("quit", KeyCode::Escape);
        bindings.bind_action("reset", KeyCode::KeyR);
        bindings.bind_action("stats", KeyCode::KeyC);
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

        // 程序化生成的棋盘格贴图，直接登记为资源，无需外部图片文件。
        let checker = ctx.resources.register(
            "builtin/checker",
            Texture::checkerboard(64, 8, [230, 230, 235, 255], [40, 44, 60, 255])
                .with_sampler(Sampler::pixelated()),
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

        // 贴了棋盘格的立方体，金属度高、粗糙度低 → 高光锐利。
        self.cube = ctx.scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(
                    Material::standard()
                        .with_base_color_texture(checker.clone())
                        .with_metallic(0.9)
                        .with_roughness(0.25),
                ),
        );

        // 子节点跟随父节点一起转，体现场景图层级。橙色、无贴图。
        self.orbit = ctx.scene.add_node_with_parent(
            Node::new("Orbit")
                .with_mesh(Mesh::cube())
                .with_material(
                    Material::standard()
                        .with_base_color(Vec4::new(1.0, 0.45, 0.1, 1.0))
                        .with_roughness(0.6),
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

        klog::info!("WASD 移动，Q/E 升降，空格暂停，R 重置，C 打印剔除统计，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
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
        if !self.stats_reported && ctx.elapsed > 2.0 {
            self.stats_reported = true;
            klog::info!(
                "渲染统计：绘制 {} / 剔除 {} / 共 {}，三角形 {}",
                ctx.stats.drawn,
                ctx.stats.culled,
                ctx.stats.total(),
                ctx.stats.triangles
            );
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
            let stats = ctx.stats;
            klog::info!(
                "渲染统计：绘制 {} / 剔除 {} / 共 {}，三角形 {}",
                stats.drawn,
                stats.culled,
                stats.total(),
                stats.triangles
            );
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

        if !self.paused {
            ctx.scene[self.cube].transform.rotate_y(spin_speed * ctx.dt);
            ctx.scene[self.orbit].transform.rotate_x(spin_speed * 2.0 * ctx.dt);
        }
    }
}

fn main() {
    klog::init(None);

    // 除了完整插件，也可以往指定阶段直接挂一段逻辑。
    // 这里演示在帧末统计平均帧率——物理、动画这类系统同样按这种方式接入。
    let mut frames = 0u32;
    let mut next_report = 5.0;

    App::new()
        .with_title("kengine demo")
        .add_plugin(Game::default())
        .add_system(Stage::FrameEnd, move |ctx| {
            frames += 1;
            if ctx.elapsed >= next_report {
                klog::info!("平均帧率：{:.0} FPS", frames as f32 / ctx.elapsed);
                next_report += 5.0;
            }
        })
        .run();
}
