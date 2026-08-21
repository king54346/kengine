//! 粒子：一座喷泉，粒子会从地面弹开。
//!
//! ```bash
//! cargo run --example particles
//! ```
//!
//! 空格喷一阵，C 开关碰撞，H 看粒子系统的包围盒。
//!
//! # 世界空间 vs 局部空间
//!
//! [`Space`] 决定粒子出生之后跟不跟着发射器走：
//!
//! - `World`：出生时一次性搬到世界空间，此后与节点再无关系。
//!   喷泉、火花、烟——发射器移动时，已经喷出去的不该跟着平移。
//! - `Local`：粒子始终在节点的局部空间里。护盾、拖尾、引擎尾焰——
//!   它们本来就该跟着物体走。
//!
//! 选错的表现很好认：拖着发射器跑，World 的粒子留在原地拖出一条尾巴，
//! Local 的粒子整团跟着走。

use kengine::prelude::*;

#[derive(Default)]
struct ParticleDemo {
    fountain: Handle<Node>,
    collide: bool,
    angle: f32,
}

impl Plugin for ParticleDemo {
    fn init(&mut self, ctx: &mut Context) {
        ctx.input
            .bindings_mut()
            .bind_action("burst", KeyCode::Space);
        ctx.input
            .bindings_mut()
            .bind_action("collide", KeyCode::KeyC);
        ctx.input
            .bindings_mut()
            .bind_action("gizmos", KeyCode::KeyH);

        ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 3.0, 8.0),
                    Vec3::new(0.0, 1.5, 0.0),
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(2.0))
                .with_transform(Transform::looking_at(
                    Vec3::new(3.0, 6.0, 4.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Ground")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::splat(0.3), 0.85))
                .with_scale(Vec3::new(16.0, 0.2, 16.0))
                .with_position(Vec3::new(0.0, -0.1, 0.0)),
        );

        let system = ParticleSystem::new(
            Emitter::default()
                .with_rate(600.0)
                .with_direction(Vec3::Y)
                // 张角为零就是一条直线，看不出是粒子还是一根柱子。
                .with_spread_degrees(18.0)
                .with_speed((5.0, 7.0))
                .with_lifetime((1.6, 2.4))
                .with_size((0.06, 0.14)),
        )
        .with_capacity(4_000)
        // 重力朝下：粒子的加速度和物理世界是两套，互不影响。
        .with_acceleration(Vec3::new(0.0, -9.0, 0.0))
        // 末尾淡出到全透明，否则粒子会「啪」地一下消失。
        .with_color(ColorGradient::new(
            [
                (0.0, Vec4::new(0.4, 0.8, 1.0, 1.0)),
                (0.6, Vec4::new(0.2, 0.4, 1.0, 0.8)),
                (1.0, Vec4::new(0.1, 0.1, 0.6, 0.0)),
            ],
            // 关键帧取不到值时的兜底色。
            Vec4::new(0.4, 0.8, 1.0, 1.0),
        ))
        .with_space(Space::World);

        self.fountain = ctx
            .scene
            .add_node(Node::new("Fountain").with_particles(system));

        klog::info!("空格喷一阵，C 开关地面碰撞，H 看包围盒，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        // 让发射器绕圈：World 空间下，喷出去的粒子会留在原地拖出一条弧。
        self.angle += ctx.dt * 0.7;
        if let Some(node) = ctx.scene.try_get_mut(self.fountain) {
            node.transform.position =
                Vec3::new(self.angle.cos() * 1.5, 0.2, self.angle.sin() * 1.5);
        }

        if ctx.input.action_just_pressed("burst") {
            // 世界空间的粒子出生时要用节点的世界变换定位，所以爆发要把矩阵传进去。
            // 先取矩阵再取系统：两者都借 scene，反过来写借用检查过不了。
            let world = ctx.scene.world_matrix(self.fountain);
            if let Some(system) = ctx
                .scene
                .try_get_mut(self.fountain)
                .and_then(Node::particles_mut)
            {
                system.burst(400, world);
                klog::info!("喷了 400 个");
            }
        }

        if ctx.input.action_just_pressed("collide") {
            self.collide = !self.collide;
            if let Some(system) = ctx
                .scene
                .try_get_mut(self.fountain)
                .and_then(Node::particles_mut)
            {
                // 粒子碰撞用的是**无限大平面**，不是物理世界的碰撞体：
                // 几万个粒子逐个去查询物理世界代价太高。真要跟实际几何碰，
                // 用 `Collision::scene()`——它按预算轮转，不是每颗每帧都查。
                system.collision = self
                    .collide
                    .then(|| Collision::ground(0.0).with_response(CollisionResponse::bouncy()));
            }
            klog::info!("地面碰撞{}", if self.collide { "开" } else { "关" });
        }

        if ctx.input.action_just_pressed("gizmos") {
            let on = ctx.scene.gizmos_mut().toggle();
            ctx.debug.scene.bounds = on;
            klog::info!("包围盒{}", if on { "开" } else { "关" });
        }
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — particles")
        .add_plugin(ParticleDemo::default())
        .run();
}
