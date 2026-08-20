//! 物理：一摞箱子、一发射线、一次冲量。
//!
//! ```bash
//! cargo run --example physics_basic
//! ```
//!
//! 空格开火（射线拾取 + 冲量），R 重码，G 开关重力，H 看碰撞体线框。
//!
//! # 两个容易踩的坑
//!
//! 一、**刚体和碰撞体是两个组件**，挂在同一个节点上。只有刚体没有碰撞体的
//! 物体质量为零，冲量对它不起作用（`Δv = 冲量 × 质量倒数`）——看起来像
//! 「施力没反应」，其实是它还没有形状。
//!
//! 二、**物理写的是节点的局部变换**，所以顺序是「先步进物理，再算世界变换」。
//! 引擎的帧循环已经排好了，但自己往 `Stage::Physics` 里挂东西时要记得。

use kengine::prelude::*;

/// 一摞几个、几摞。
const STACK_HEIGHT: usize = 5;
const STACK_COUNT: usize = 3;

#[derive(Default)]
struct PhysicsDemo {
    crates: Vec<Handle<Node>>,
    camera: Handle<Node>,
    gravity_on: bool,
}

impl PhysicsDemo {
    /// 把箱子摆回初始位置，并清掉速度。
    ///
    /// 只改变换是不够的：刚体还带着上一次的线速度与角速度，
    /// 摆回去之后它会立刻按老速度继续飞。
    fn restack(&mut self, ctx: &mut Context) {
        for (i, handle) in self.crates.iter().enumerate() {
            let (stack, height) = (i / STACK_HEIGHT, i % STACK_HEIGHT);
            let Some(node) = ctx.scene.try_get_mut(*handle) else {
                continue;
            };
            node.transform.position =
                Vec3::new(stack as f32 * 1.5 - 1.5, 0.55 + height as f32 * 1.02, 0.0);
            node.transform.rotation = Quat::IDENTITY;
            if let Some(body) = node.rigid_body_mut() {
                body.set_linvel(Vec3::ZERO);
                body.set_angvel(Vec3::ZERO);
                // 睡着的刚体不参与求解，摆回去也不会重新落下来。
                body.wake_up();
            }
        }
        klog::info!("箱子已重码");
    }

    /// 从相机往正前方打一条射线，命中的刚体挨一发冲量。
    fn shoot(&mut self, ctx: &mut Context) {
        let Some(camera) = ctx.scene.try_get(self.camera) else {
            return;
        };
        let matrix = camera.global_transform();
        let origin = matrix.w_axis.truncate();
        // 本引擎（和 glTF）的约定：前方是 -Z。
        let forward = -matrix.z_axis.truncate().normalize_or_zero();

        let Some(hit) = ctx
            .scene
            .cast_ray(&RayCastOptions::new(origin, forward, 100.0))
        else {
            klog::info!("打空了");
            return;
        };

        // 射线命中的是**碰撞体**；要施力得找到它所属的刚体节点。
        // 两者常常是同一个节点，但碰撞体挂在子节点上时就不是了。
        let Some(body_node) = hit.body_node else {
            klog::info!("打中了静态物体");
            return;
        };
        let Some(node) = ctx.scene.try_get_mut(body_node) else {
            return;
        };
        let Some(body) = node.rigid_body_mut() else {
            return;
        };
        body.apply_impulse(forward * 8.0);
        klog::info!("命中，距离 {:.2} 米", hit.distance);
    }
}

impl Plugin for PhysicsDemo {
    fn init(&mut self, ctx: &mut Context) {
        ctx.input
            .bindings_mut()
            .bind_action("shoot", KeyCode::Space);
        ctx.input
            .bindings_mut()
            .bind_action("restack", KeyCode::KeyR);
        ctx.input
            .bindings_mut()
            .bind_action("gravity", KeyCode::KeyG);
        ctx.input
            .bindings_mut()
            .bind_action("gizmos", KeyCode::KeyH);

        self.camera = ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 3.0, 9.0),
                    Vec3::new(0.0, 1.5, 0.0),
                    Vec3::Y,
                )),
        );

        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(3.0).with_shadows())
                .with_transform(Transform::looking_at(
                    Vec3::new(4.0, 8.0, 5.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        // 地面：只有碰撞体，没有刚体 = 静态。不动的东西不必进求解器。
        ctx.scene.add_node(
            Node::new("Ground")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::splat(0.35), 0.9))
                .with_scale(Vec3::new(20.0, 0.2, 20.0))
                .with_collider(Collider::cuboid(Vec3::new(10.0, 0.1, 10.0)))
                .with_position(Vec3::new(0.0, -0.1, 0.0)),
        );

        for stack in 0..STACK_COUNT {
            for height in 0..STACK_HEIGHT {
                let handle = ctx.scene.add_node(
                    Node::new(format!("Crate{stack}_{height}"))
                        .with_mesh(Mesh::cube())
                        .with_material(PbrMaterial::metal(Vec3::new(0.8, 0.45, 0.2), 0.5))
                        .with_rigid_body(RigidBody::dynamic())
                        // 碰撞体的半长要和网格对得上。对不上的话画面里
                        // 箱子会悬空或者陷进地里——而且不报任何错。
                        .with_collider(Collider::cuboid(Vec3::splat(0.5)))
                        .with_position(Vec3::new(
                            stack as f32 * 1.5 - 1.5,
                            0.55 + height as f32 * 1.02,
                            0.0,
                        )),
                );
                self.crates.push(handle);
            }
        }

        self.gravity_on = true;
        klog::info!("空格开火，R 重码，G 开关重力，H 看碰撞体线框，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        if ctx.input.action_just_pressed("shoot") {
            self.shoot(ctx);
        }
        if ctx.input.action_just_pressed("restack") {
            self.restack(ctx);
        }
        if ctx.input.action_just_pressed("gravity") {
            self.gravity_on = !self.gravity_on;
            let g = if self.gravity_on {
                Vec3::new(0.0, -9.81, 0.0)
            } else {
                Vec3::ZERO
            };
            ctx.scene.physics_mut().set_gravity(g);
            klog::info!("重力{}", if self.gravity_on { "开" } else { "关" });
        }
        if ctx.input.action_just_pressed("gizmos") {
            let on = ctx.scene.gizmos_mut().toggle();
            // 总开关和「画哪些」是两件事：前者关着时后者一个都不生效。
            ctx.debug.physics = if on {
                PhysicsDebugOptions::default()
            } else {
                PhysicsDebugOptions::none()
            };
            klog::info!("碰撞体线框{}", if on { "开" } else { "关" });
        }
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — physics basic")
        .add_plugin(PhysicsDemo::default())
        .run();
}
