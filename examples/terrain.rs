//! 地形：高度图、LOD 分块、笔刷编辑、高度场碰撞。
//!
//! ```bash
//! cargo run --example terrain
//! ```
//!
//! WASD 移动相机，鼠标左键抬升、右键下压、中键抹平，
//! `[` `]` 改笔刷半径，H 看分块与包围盒，空格丢一个球下去试碰撞。
//!
//! # 块是普通子节点
//!
//! 每块地形对应一个挂着网格的普通节点。于是剔除、批处理、阴影
//! 全都白捡——渲染器根本不知道这些网格是地形生成的。
//!
//! # 碰撞体是高度场，不是三角网格
//!
//! 三角网格要把每个三角形都建进物理世界；高度场只存高度值。
//! 一块 1024² 的地形，前者两百万个三角形，后者一百万个 `f32`。
//!
//! 注意编辑之后碰撞体**不会自动跟着变** —— 重建高度场碰撞体不便宜，
//! 什么时候重建是调用方的决定。这个例子在松开鼠标时重建一次。

use kengine::kterrain::{Brush, Heightmap, Operation};
use kengine::prelude::*;

/// 地形尺寸（米）与顶点分辨率。
const SIZE: f32 = 400.0;
const RESOLUTION: usize = 129;

#[derive(Default)]
struct TerrainDemo {
    terrain: Handle<Node>,
    camera: Handle<Node>,
    radius: f32,
    editing: bool,
    balls: Vec<Handle<Node>>,
}

impl TerrainDemo {
    /// 造一片有起伏的地形。
    fn make_heightmap() -> Heightmap {
        let mut map = Heightmap::flat(RESOLUTION, RESOLUTION, Vec2::new(SIZE, SIZE));
        for row in 0..map.rows() {
            for col in 0..map.cols() {
                let x = col as f32 / map.cols() as f32;
                let z = row as f32 / map.rows() as f32;
                // 几层不同频率叠起来，比单层正弦像地形一点。
                let h = (x * 6.0).sin() * 12.0
                    + (z * 4.5).cos() * 9.0
                    + (x * 17.0).sin() * (z * 13.0).cos() * 3.0;
                map.set_height(col, row, h);
            }
        }
        map
    }

    /// 从屏幕中心往前打一条射线，落在地形上的点就是笔刷中心。
    ///
    /// 用地形自己的射线而不是物理射线：地形被编辑之后物理碰撞体
    /// 还是旧的，用物理射线会让笔刷落在上一次重建时的地面上。
    fn brush_target(&self, ctx: &Context) -> Option<Vec3> {
        let camera = ctx.scene.try_get(self.camera)?;
        let matrix = camera.global_transform();
        let origin = matrix.w_axis.truncate();
        let forward = -matrix.z_axis.truncate().normalize_or_zero();
        ctx.scene
            .raycast_terrain(origin, forward, 1000.0)
            .map(|(_, point)| point)
    }

    /// 落一笔。
    fn paint(&mut self, ctx: &mut Context, operation: Operation) {
        let Some(point) = self.brush_target(ctx) else {
            return;
        };
        // 世界坐标换算到地形局部坐标。地形放在原点，所以这里其实一样，
        // 但写出来免得挪动地形之后笔刷跑偏。
        let to_local = ctx.scene.world_matrix(self.terrain).inverse();
        let local = to_local.transform_point3(point);

        let strength = match operation {
            // 抬升/下压按米算，抹平/压平按比例算——两者量纲不同，
            // 用同一个强度值的话抹平会几乎看不出效果。
            Operation::Raise | Operation::Lower => 0.6,
            Operation::Smooth | Operation::Flatten(_) => 0.25,
        };
        let brush = Brush {
            center: Vec2::new(local.x, local.z),
            radius: self.radius,
            strength,
            falloff: 0.6,
        };

        if let Some(terrain) = ctx
            .scene
            .try_get_mut(self.terrain)
            .and_then(Node::terrain_mut)
        {
            kengine::kterrain::apply(terrain.heightmap_mut(), &brush, operation);
        }
        self.editing = true;
    }
}

impl Plugin for TerrainDemo {
    fn init(&mut self, ctx: &mut Context) {
        let b = ctx.input.bindings_mut();
        b.bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
        b.bind_axis("forward", KeyCode::KeyW, KeyCode::KeyS);
        b.bind_axis("vertical", KeyCode::KeyE, KeyCode::KeyQ);
        b.bind_action("gizmos", KeyCode::KeyH);
        b.bind_action("drop", KeyCode::Space);
        b.bind_action("smaller", KeyCode::BracketLeft);
        b.bind_action("bigger", KeyCode::BracketRight);

        self.radius = 25.0;

        self.camera = ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(SIZE * 0.5, 90.0, SIZE * 0.5 + 120.0),
                    Vec3::new(SIZE * 0.5, 0.0, SIZE * 0.5),
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(3.0).with_shadows())
                .with_transform(Transform::looking_at(
                    Vec3::new(200.0, 300.0, 150.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        // 材质挂在地形节点上，块子节点会继承它。
        self.terrain = ctx.scene.add_node(
            Node::new("Terrain")
                .with_terrain(Terrain::new(Self::make_heightmap(), 32, 3))
                .with_material(PbrMaterial::metal(Vec3::new(0.35, 0.45, 0.28), 0.85)),
        );
        // 先 update 一次把块生成出来，再装碰撞体。
        ctx.scene.update();
        ctx.scene.attach_terrain_collider(self.terrain);

        klog::info!(
            "地形 {RESOLUTION}×{RESOLUTION} 顶点，{SIZE}×{SIZE} 米，切成 {} 块",
            ctx.scene
                .try_get(self.terrain)
                .and_then(Node::terrain)
                .map_or(0, |t| t.chunks().len())
        );
        // TEMP-VALIDATION：HDR + 两个反射探针。
        {
            let (w, h) = (128usize, 64usize);
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"#?RADIANCE
FORMAT=32-bit_rle_rgbe

");
            bytes.extend_from_slice(format!("-Y {h} +X {w}
").as_bytes());
            for row in 0..h {
                for _ in 0..w {
                    let p = if row < h / 2 { [90u8, 140, 255, 129] } else { [60, 55, 45, 126] };
                    bytes.extend_from_slice(&p);
                }
            }
            let image = kengine::kpbr::hdr::HdrImage::decode(&bytes).unwrap();
            ctx.scene.set_environment_hdr(
                &image,
                kengine::kpbr::prefilter::PrefilterSettings {
                    base_width: 64, levels: 4, samples: 32,
                },
            );
            use kengine::kpbr::probe::ReflectionProbe;
            let a = ctx.scene.add_reflection_probe(
                ReflectionProbe::new(Vec3::new(150.0, 0.0, 150.0), Vec3::splat(400.0)),
                &image,
            );
            let b = ctx.scene.add_reflection_probe(
                ReflectionProbe::new(Vec3::new(150.0, 0.0, 150.0), Vec3::splat(120.0)),
                &image,
            );
            klog::info!(
                "[验证] 探针 a={a:?} b={b:?}，共 {} 个，纹理数组 {} 层",
                ctx.scene.reflection_probes().len(),
                ctx.scene.reflection_probes().len() + 1
            );
            klog::info!(
                "[验证] probe_at(150,0,150)={:?}  probe_at(350,0,50)={:?}  probe_at(0,0,0)={:?}",
                ctx.scene.probe_at(Vec3::new(150.0, 0.0, 150.0)),
                ctx.scene.probe_at(Vec3::new(350.0, 0.0, 50.0)),
                ctx.scene.probe_at(Vec3::ZERO)
            );
        }

        klog::info!("WASD/QE 移动，左键抬升、右键下压、中键抹平，[ ] 改笔刷，H 看分块，空格丢球");
    }

    fn update(&mut self, ctx: &mut Context) {
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }

        // 相机。
        let (strafe, forward, lift) = (
            ctx.input.axis("horizontal"),
            ctx.input.axis("forward"),
            ctx.input.axis("vertical"),
        );
        if let Some(node) = ctx.scene.try_get_mut(self.camera) {
            let matrix = node.global_transform();
            let right = matrix.x_axis.truncate().normalize_or_zero();
            let ahead = -matrix.z_axis.truncate().normalize_or_zero();
            let speed = 60.0 * ctx.dt;
            node.transform.position += (right * strafe + ahead * forward + Vec3::Y * lift) * speed;
        }

        if ctx.input.action_just_pressed("smaller") {
            self.radius = (self.radius - 5.0).max(5.0);
        }
        if ctx.input.action_just_pressed("bigger") {
            self.radius = (self.radius + 5.0).min(80.0);
        }

        // 笔刷。
        if ctx.input.mouse_pressed(MouseButton::Left) {
            self.paint(ctx, Operation::Raise);
        } else if ctx.input.mouse_pressed(MouseButton::Right) {
            self.paint(ctx, Operation::Lower);
        } else if ctx.input.mouse_pressed(MouseButton::Middle) {
            self.paint(ctx, Operation::Smooth);
        } else if self.editing {
            // 松手才重建碰撞体：高度场碰撞体是整块重建的，
            // 编辑期间每帧重建会明显卡顿。
            self.editing = false;
            ctx.scene.attach_terrain_collider(self.terrain);
            klog::info!("碰撞体已重建");
        }

        // 丢球试碰撞。
        if ctx.input.action_just_pressed("drop")
            && let Some(point) = self.brush_target(ctx)
        {
            let ball = ctx.scene.add_node(
                Node::new(format!("Ball{}", self.balls.len()))
                    .with_mesh(Mesh::sphere(12, 16))
                    .with_material(PbrMaterial::metal(Vec3::new(0.9, 0.3, 0.2), 0.3))
                    .with_scale(Vec3::splat(2.0))
                    .with_rigid_body(RigidBody::dynamic())
                    .with_collider(Collider::ball(2.0))
                    .with_position(point + Vec3::Y * 40.0),
            );
            self.balls.push(ball);
        }

        // 笔刷落点画个圈。
        if ctx.scene.gizmos().enabled()
            && let Some(point) = self.brush_target(ctx)
        {
            let normal = ctx
                .scene
                .try_get(self.terrain)
                .and_then(Node::terrain)
                .map_or(Vec3::Y, |t| t.heightmap().normal(point.x, point.z));
            let gizmos = ctx.scene.gizmos_mut();
            gizmos.on_top(|g| {
                g.circle(point + normal * 0.2, normal, self.radius, GizmoColor::CYAN);
                g.arrow(point, point + normal * 8.0, GizmoColor::YELLOW);
            });
        }

        if ctx.input.action_just_pressed("gizmos") {
            let on = ctx.scene.gizmos_mut().toggle();
            ctx.debug.scene.bounds = on;
            klog::info!("分块包围盒{}", if on { "开" } else { "关" });
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — terrain")
        .add_plugin(TerrainDemo::default())
        .run();
}
