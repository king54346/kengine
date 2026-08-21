//! 2D 物理：刚体、碰撞、传感器、射线，用精灵管线画出来。
//!
//! ```bash
//! cargo run --example physics_2d
//! ```
//!
//! 左键在鼠标处丢一个盒子，右键丢一个球，R 重来，空格暂停。
//!
//! # 2D 物理和 3D 物理是两个世界
//!
//! [`Scene::physics2d`] 和 [`Scene::physics`] 互不感知，各自步进。
//! 一个 2D 刚体永远不会撞到一个 3D 刚体。
//!
//! # 为什么不用节点
//!
//! 这个引擎的 2D 走**立即模式精灵**——每帧声明画什么，不建节点。
//! 所以这里的做法是直接读刚体位置去发精灵，中间不经过场景图。
//! 想用节点的话有 `Scene::sync_node_from_2d_body`。

use kengine::kphysics::d2;
use kengine::prelude::*;

/// 世界有多宽（世界单位）。
const WORLD_WIDTH: f32 = 24.0;
/// 地面在哪儿。
const GROUND_Y: f32 = -8.0;

/// 一个会动的物体：刚体 + 画它要用的东西。
struct Prop {
    body: d2::BodyHandle,
    /// 半长（盒子）或半径（球）。画的时候要用。
    half_size: Vec2,
    color: Vec4,
    ball: bool,
}

#[derive(Default)]
struct Physics2d {
    camera: Handle<Node>,
    texture: Option<kengine::kcore::uuid::Uuid>,
    props: Vec<Prop>,
    /// 落进这个区域的东西会被记一笔。传感器不产生碰撞响应，
    /// 所以物体会直接穿过去。
    goal: Option<d2::ColliderHandle>,
    scored: usize,
    paused: bool,
    /// 从相机往下打的射线命中点，用来演示射线查询。
    ray_hit: Option<Vec2>,
}

/// 一张 1×1 的白色贴图。精灵靠顶点色上色，所以贴图只要是白的就够了。
fn white_texture() -> Texture {
    Texture::new(1, 1, vec![255, 255, 255, 255])
}

impl Physics2d {
    /// 建地面和两侧的墙。
    fn build_walls(&mut self, scene: &mut Scene) {
        let world = scene.physics2d_mut();
        let half = WORLD_WIDTH * 0.5;

        // 地面。
        let ground = world.add_body(
            &d2::RigidBodyDesc::fixed().with_position(Vec2::new(0.0, GROUND_Y)),
            0,
        );
        world
            .add_collider(
                &d2::ColliderDesc::cuboid(Vec2::new(half, 0.5)).with_friction(0.8),
                Some(ground),
                0,
            )
            .expect("地面");

        // 两侧的墙，不然东西会滚出屏幕。
        for x in [-half, half] {
            let wall = world.add_body(
                &d2::RigidBodyDesc::fixed().with_position(Vec2::new(x, 0.0)),
                0,
            );
            world
                .add_collider(&d2::ColliderDesc::cuboid(Vec2::new(0.5, 12.0)), Some(wall), 0)
                .expect("墙");
        }

        // 一个斜坡，用折线做——2D 关卡的地形轮廓就是这么搭的。
        let slope = world.add_body(
            &d2::RigidBodyDesc::fixed().with_position(Vec2::new(-4.0, GROUND_Y + 0.5)),
            0,
        );
        world
            .add_collider(
                &d2::ColliderDesc::polyline(vec![
                    Vec2::new(-6.0, 5.0),
                    Vec2::new(0.0, 0.0),
                    Vec2::new(6.0, 0.0),
                ]),
                Some(slope),
                0,
            )
            .expect("斜坡");

        // 传感器：只报告重叠，不挡路。
        let goal_body = world.add_body(
            &d2::RigidBodyDesc::fixed().with_position(Vec2::new(8.0, GROUND_Y + 1.5)),
            0,
        );
        self.goal = world.add_collider(
            &d2::ColliderDesc::cuboid(Vec2::new(2.0, 1.0)).as_sensor(),
            Some(goal_body),
            0,
        );
    }

    /// 丢一个物体。
    fn spawn(&mut self, scene: &mut Scene, at: Vec2, ball: bool) {
        let half_size = if ball {
            Vec2::splat(0.35)
        } else {
            Vec2::new(0.4, 0.4)
        };

        let world = scene.physics2d_mut();
        let body = world.add_body(
            &d2::RigidBodyDesc::dynamic()
                .with_position(at)
                // 给一点初速度，看着更活。
                .with_linvel(Vec2::new((self.props.len() % 7) as f32 - 3.0, 0.0)),
            self.props.len() as u128,
        );

        let desc = if ball {
            d2::ColliderDesc::ball(half_size.x).with_restitution(0.6)
        } else {
            d2::ColliderDesc::cuboid(half_size).with_friction(0.6)
        };
        if world.add_collider(&desc, Some(body), 0).is_none() {
            // 形状退化时 `add_collider` 返回 None。这里不会发生，
            // 但漏掉的话会留下一个没有碰撞体的刚体，它会直接穿过地面。
            world.remove_body(body);
            klog::warn!("碰撞体建不出来，刚体已回收");
            return;
        }

        // 用色相环上等距的颜色，相邻的物体好区分。
        let hue = self.props.len() as f32 * 0.618;
        let color = Vec4::new(
            (hue * 6.28).sin() * 0.4 + 0.6,
            (hue * 6.28 + 2.1).sin() * 0.4 + 0.6,
            (hue * 6.28 + 4.2).sin() * 0.4 + 0.6,
            1.0,
        );

        self.props.push(Prop {
            body,
            half_size,
            color,
            ball,
        });
    }

    /// 清掉所有动态物体，墙留着。
    fn reset(&mut self, scene: &mut Scene) {
        for prop in self.props.drain(..) {
            scene.physics2d_mut().remove_body(prop.body);
        }
        self.scored = 0;
    }
}

impl Plugin for Physics2d {
    fn init(&mut self, ctx: &mut Context) {
        self.camera = ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                // 2D 世界躺在 XY 平面上，相机沿 -Z 看过去。
                .with_position(Vec3::new(0.0, 0.0, 22.0)),
        );

        let texture = white_texture();
        self.texture = Some(texture.id());
        ctx.scene.register_sprite_texture(texture);

        self.build_walls(&mut ctx.scene);

        // 一开始先堆一摞盒子。
        for i in 0..12 {
            let x = (i % 4) as f32 * 1.0 - 1.5;
            let y = GROUND_Y + 1.0 + (i / 4) as f32 * 1.0;
            self.spawn(&mut ctx.scene, Vec2::new(x, y), i % 3 == 0);
        }

        let b = ctx.input.bindings_mut();
        b.bind_action("spawn_box", MouseButton::Left);
        b.bind_action("spawn_ball", MouseButton::Right);
        b.bind_action("reset", KeyCode::KeyR);
        b.bind_action("pause", KeyCode::Space);

        klog::info!("左键丢盒子，右键丢球，R 重来，空格暂停");
    }

    fn update(&mut self, ctx: &mut Context) {
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
        if ctx.input.action_just_pressed("pause") {
            self.paused = !self.paused;
            klog::info!("{}", if self.paused { "暂停" } else { "继续" });
        }
        if ctx.input.action_just_pressed("reset") {
            self.reset(&mut ctx.scene);
            klog::info!("已重置");
        }

        // 鼠标位置换算到世界坐标。
        let cursor = ctx.input.cursor_position().unwrap_or(Vec2::ZERO);
        let size = {
            let s = ctx.window.inner_size();
            Vec2::new(s.width.max(1) as f32, s.height.max(1) as f32)
        };
        let world_position = Vec2::new(
            (cursor.x / size.x - 0.5) * WORLD_WIDTH,
            // 屏幕 Y 朝下，世界 Y 朝上。
            (0.5 - cursor.y / size.y) * WORLD_WIDTH * size.y / size.x,
        );

        if ctx.input.action_just_pressed("spawn_box") {
            self.spawn(&mut ctx.scene, world_position, false);
        }
        if ctx.input.action_just_pressed("spawn_ball") {
            self.spawn(&mut ctx.scene, world_position, true);
        }

        if !self.paused {
            ctx.scene.step_physics_2d(ctx.dt);

            // 传感器事件：落进目标区的记一笔。
            if let Some(goal) = self.goal {
                let hits = ctx
                    .scene
                    .physics2d()
                    .collision_events()
                    .iter()
                    .filter(|e| e.started && (e.first == goal || e.second == goal))
                    .count();
                if hits > 0 {
                    self.scored += hits;
                    klog::info!("进球 {}（累计 {}）", hits, self.scored);
                }
            }
        }

        // 从鼠标处往下打一条射线，演示查询。
        self.ray_hit = ctx
            .scene
            .physics2d_mut()
            .cast_ray(&d2::RayCastOptions {
                origin: world_position,
                direction: Vec2::new(0.0, -1.0),
                max_distance: 40.0,
                ..Default::default()
            })
            .map(|hit| hit.point);

        let Some(texture) = self.texture else {
            return;
        };

        // ── 画 ──
        //
        // 立即模式：每帧重新提交，不提交就不画。

        // 地面和墙。
        let half = WORLD_WIDTH * 0.5;
        let wall_color = Vec4::new(0.25, 0.28, 0.33, 1.0);
        ctx.scene.push_sprite(
            SpriteInstance::new(Vec2::new(0.0, GROUND_Y), Vec2::new(WORLD_WIDTH, 1.0), texture)
                .with_color(wall_color),
        );
        for x in [-half, half] {
            ctx.scene.push_sprite(
                SpriteInstance::new(Vec2::new(x, 0.0), Vec2::new(1.0, 24.0), texture)
                    .with_color(wall_color),
            );
        }

        // 目标区（传感器）画成半透明。
        ctx.scene.push_sprite(
            SpriteInstance::new(
                Vec2::new(8.0, GROUND_Y + 1.5),
                Vec2::new(4.0, 2.0),
                texture,
            )
            .with_color(Vec4::new(0.3, 0.9, 0.4, 0.35)),
        );

        // 物体。位置和角度直接从刚体读。
        for prop in &self.props {
            let Some(body) = ctx.scene.physics2d().body(prop.body) else {
                continue;
            };
            let position = body.position();
            let rotation = body.rotation();

            ctx.scene.push_sprite(
                SpriteInstance::new(position, prop.half_size * 2.0, texture)
                    .with_color(prop.color)
                    .with_rotation(rotation)
                    .with_layer(1),
            );
            // 球画小一点的内芯，好看出它在滚。
            if prop.ball {
                let marker = position + Vec2::new(rotation.cos(), rotation.sin()) * 0.18;
                ctx.scene.push_sprite(
                    SpriteInstance::new(marker, Vec2::splat(0.12), texture)
                        .with_color(Vec4::new(0.1, 0.1, 0.12, 1.0))
                        .with_layer(2),
                );
            }
        }

        // TEMP-VALIDATION
        {
            static mut N: u32 = 0;
            let n = unsafe { N += 1; N };
            if n == 120 {
                let live = self.props.iter().filter(|p| ctx.scene.physics2d().body(p.body).is_some()).count();
                let resting = self.props.iter().filter(|p| {
                    ctx.scene.physics2d().body(p.body).map_or(false, |b| b.linvel().length() < 0.1)
                }).count();
                let lowest = self.props.iter().filter_map(|p| {
                    ctx.scene.physics2d().body(p.body).map(|b| b.position().y)
                }).fold(f32::MAX, f32::min);
                let finite = self.props.iter().all(|p| {
                    ctx.scene.physics2d().body(p.body).map_or(true, |b| b.position().is_finite())
                });
                klog::info!(
                    "[验证] 刚体 {live} 个，静止 {resting} 个，最低 y={lowest:.2}（地面 {:.1}），全部有限={finite}，精灵 {}，射线命中={:?}",
                    GROUND_Y + 0.5, ctx.stats.sprites, self.ray_hit.is_some()
                );
            }
        }

        // 射线命中点。
        if let Some(point) = self.ray_hit {
            ctx.scene.push_sprite(
                SpriteInstance::new(point, Vec2::splat(0.25), texture)
                    .with_color(Vec4::new(1.0, 0.9, 0.2, 1.0))
                    .with_layer(3),
            );
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine —— 2D 物理")
        .add_plugin(Physics2d::default())
        .run();
}
