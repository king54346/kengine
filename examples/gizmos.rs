//! 调试绘制：所有形状 + 内置叠加层。
//!
//! ```bash
//! cargo run --example gizmos
//! ```
//!
//! H 总开关，1 物理线框，2 包围盒，3 BVH，4 骨架/光源/相机。
//!
//! # 即时模式
//!
//! 没有句柄、没有生命周期管理：**想让一条线一直在，就每帧都画它**。
//! 引擎在帧末清空缓冲，所以不画就没了。这听着笨，但它消掉了调试绘制
//! 最烦人的那类问题——「删了代码但线还在」和「东西没了线还留着」。
//!
//! # 两层
//!
//! - 深度层：参与深度测试，被物体挡住。看空间关系用它。
//! - 覆盖层（`on_top`）：永远画在最上面。找「东西到底在哪」用它——
//!   要调试的物体常常正好埋在别的东西里面。
//!
//! # 总开关关着时一切免费
//!
//! `Gizmos::enabled` 为假时所有绘制方法直接返回，连形状都不算。
//! 所以调试绘制的调用可以放心散落在游戏逻辑里，发布版本不必删。

use kengine::prelude::*;

#[derive(Default)]
struct GizmoDemo {
    elapsed: f32,
    physics_on: bool,
}

impl GizmoDemo {
    /// 一排静态展示：每个形状画一个。
    fn draw_gallery(&self, gizmos: &mut Gizmos) {
        let y = 0.8;
        gizmos.sphere(Vec3::new(-6.0, y, 0.0), 0.6, GizmoColor::CYAN);
        gizmos.capsule(
            Vec3::new(-4.5, 0.3, 0.0),
            Vec3::new(-4.5, 1.4, 0.0),
            0.4,
            GizmoColor::GREEN,
        );
        gizmos.cylinder(
            Vec3::new(-3.0, 0.3, 0.0),
            Vec3::new(-3.0, 1.4, 0.0),
            0.45,
            GizmoColor::YELLOW,
        );
        gizmos.cone(
            Vec3::new(-1.5, 0.3, 0.0),
            Vec3::new(-1.5, 1.6, 0.0),
            0.5,
            GizmoColor::ORANGE,
        );
        gizmos.aabb(
            Aabb::from_center_half_extents(Vec3::new(0.0, y, 0.0), Vec3::splat(0.5)),
            GizmoColor::WHITE,
        );
        gizmos.cuboid(
            Mat4::from_rotation_translation(
                Quat::from_rotation_y(self.elapsed),
                Vec3::new(1.5, y, 0.0),
            ),
            Vec3::splat(0.5),
            GizmoColor::MAGENTA,
        );
        gizmos.circle(Vec3::new(3.0, y, 0.0), Vec3::Y, 0.7, GizmoColor::BLUE);
        gizmos.arrow(
            Vec3::new(4.5, 0.2, 0.0),
            Vec3::new(4.5, 1.6, 0.0),
            GizmoColor::RED,
        );
        gizmos.transform(
            Mat4::from_rotation_translation(
                Quat::from_rotation_x(self.elapsed * 0.6),
                Vec3::new(6.0, y, 0.0),
            ),
            0.6,
        );

        // 地面网格：提供空间参照。中轴会自动画亮一点。
        gizmos.grid(Vec3::ZERO, 1.0, 10, GizmoColor::GRAY.scaled(0.25));
    }

    /// 一条动的曲线，演示 `polyline` 与渐变色。
    fn draw_curve(&self, gizmos: &mut Gizmos) {
        let points: Vec<Vec3> = (0..64)
            .map(|i| {
                let t = i as f32 / 63.0;
                let angle = t * std::f32::consts::TAU + self.elapsed;
                Vec3::new(
                    angle.cos() * 3.0,
                    2.5 + (t * 6.0 + self.elapsed * 2.0).sin() * 0.5,
                    angle.sin() * 3.0,
                )
            })
            .collect();
        gizmos.polyline_closed(&points, GizmoColor::CYAN.with_alpha(0.8));

        // 覆盖层：这条一定看得见，哪怕被曲线自己挡住。
        gizmos.on_top(|g| {
            for p in points.iter().step_by(8) {
                g.point(*p, 0.12, GizmoColor::WHITE);
            }
        });
    }
}

impl Plugin for GizmoDemo {
    fn init(&mut self, ctx: &mut Context) {
        let b = ctx.input.bindings_mut();
        b.bind_action("gizmos", KeyCode::KeyH);
        b.bind_action("physics", KeyCode::Digit1);
        b.bind_action("bounds", KeyCode::Digit2);
        b.bind_action("bvh", KeyCode::Digit3);
        b.bind_action("rig", KeyCode::Digit4);

        ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 6.0, 13.0),
                    Vec3::new(0.0, 1.0, 0.0),
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
        // 一盏有作用范围的点光：内置叠加层会把范围球也画出来。
        ctx.scene.add_node(
            Node::new("Bulb")
                .with_light(Light::point(5.0).with_intensity(4.0))
                .with_position(Vec3::new(-3.0, 3.0, 2.0)),
        );

        // 几个实体，让深度层的遮挡关系看得出来。
        for i in 0..6 {
            ctx.scene.add_node(
                Node::new(format!("Box{i}"))
                    .with_mesh(Mesh::cube())
                    .with_material(PbrMaterial::metal(Vec3::new(0.7, 0.5, 0.35), 0.5))
                    .with_rigid_body(RigidBody::dynamic())
                    .with_collider(Collider::cuboid(Vec3::splat(0.5)))
                    .with_position(Vec3::new(i as f32 * 1.2 - 3.0, 4.0 + i as f32, 2.0)),
            );
        }
        ctx.scene.add_node(
            Node::new("Ground")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::splat(0.25), 0.9))
                .with_scale(Vec3::new(24.0, 0.2, 24.0))
                .with_collider(Collider::cuboid(Vec3::new(12.0, 0.1, 12.0)))
                .with_position(Vec3::new(0.0, -0.1, 0.0)),
        );

        // 一部停用的相机：内置叠加层会画出它的视锥。
        // 活动相机的视锥就是屏幕本身，引擎会跳过不画。
        let mut parked = Camera::default();
        parked.enabled = false;
        ctx.scene.add_node(
            Node::new("ParkedCamera")
                .with_camera(parked)
                .with_transform(Transform::looking_at(
                    Vec3::new(-7.0, 3.0, 5.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        // 这个例子的主角就是调试绘制，所以默认开着。
        ctx.scene.gizmos_mut().set_enabled(true);
        klog::info!("H 总开关，1 物理，2 包围盒，3 BVH，4 骨架/光源/相机，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        self.elapsed += ctx.dt;

        if ctx.input.action_just_pressed("gizmos") {
            let on = ctx.scene.gizmos_mut().toggle();
            klog::info!("调试绘制{}", if on { "开" } else { "关" });
        }
        if ctx.input.action_just_pressed("physics") {
            self.physics_on = !self.physics_on;
            ctx.debug.physics = if self.physics_on {
                PhysicsDebugOptions::default()
            } else {
                PhysicsDebugOptions::none()
            };
        }
        if ctx.input.action_just_pressed("bounds") {
            ctx.debug.scene.bounds = !ctx.debug.scene.bounds;
        }
        if ctx.input.action_just_pressed("bvh") {
            ctx.debug.scene.bvh = !ctx.debug.scene.bvh;
        }
        if ctx.input.action_just_pressed("rig") {
            let on = !ctx.debug.scene.lights;
            ctx.debug.scene.lights = on;
            ctx.debug.scene.cameras = on;
            ctx.debug.scene.skeletons = on;
        }
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }

        // 手画的部分。总开关关着时这几行仍然会执行，但每个绘制调用
        // 都会立刻返回——所以不必自己加 `if enabled`。
        let gizmos = ctx.scene.gizmos_mut();
        self.draw_gallery(gizmos);
        self.draw_curve(gizmos);
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — gizmos")
        .add_plugin(GizmoDemo::default())
        .run();
}
