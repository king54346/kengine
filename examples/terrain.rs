//! 地形：高度图、LOD 分块、笔刷编辑、splat 多层材质、高度场碰撞。
//!
//! ```bash
//! cargo run --example terrain
//! ```
//!
//! WASD 移动相机，鼠标左键抬升、右键下压、中键抹平，
//! `1`~`4` 选贴图层（草/岩/土/雪），Shift + 左键涂那一层，
//! `[` `]` 改笔刷半径，H 看分块与包围盒，空格丢一个球下去试碰撞。
//!
//! # 块是普通子节点
//!
//! 每块地形对应一个挂着网格的普通节点。于是剔除、批处理、阴影
//! 全都白捡——渲染器根本不知道这些网格是地形生成的。
//!
//! # splat：材质挂在地形节点上，块子节点会继承它
//!
//! 四层地表贴图叠成一个纹理数组（`custom_texture_array`，建材质时设一次，
//! 之后不变）；混合权重是 [`SplatMap`](kengine::kterrain::SplatMap) 算出来的
//! 一张贴图（`custom_texture0`），笔刷涂过之后由 `Scene::update` 自动重传
//! ——两张贴图怎么在着色器里混合，见 `terrain_splat.wgsl`。
//!
//! # 碰撞体是高度场，不是三角网格
//!
//! 三角网格要把每个三角形都建进物理世界；高度场只存高度值。
//! 一块 1024² 的地形，前者两百万个三角形，后者一百万个 `f32`。
//!
//! 注意编辑之后碰撞体**不会自动跟着变** —— 重建高度场碰撞体不便宜，
//! 什么时候重建是调用方的决定。这个例子在松开鼠标时重建一次
//! （只有抬升/下压/抹平高度会触发重建，涂贴图层不影响碰撞体）。

use kengine::kterrain::{Brush, Heightmap, Operation};
use kengine::prelude::*;

/// 地形尺寸（米）与顶点分辨率。
const SIZE: f32 = 400.0;
const RESOLUTION: usize = 129;
/// splat 层数，恰好填满 `SplatMap::to_texture()` 的 4 个 RGBA 通道。
const LAYERS: usize = 4;

#[derive(Default)]
struct TerrainDemo {
    terrain: Handle<Node>,
    camera: Handle<Node>,
    radius: f32,
    editing: bool,
    /// 当前用 Shift + 左键涂的贴图层，0~3 对应 权重图的 R/G/B/A。
    paint_layer: usize,
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

    /// 涂一笔贴图层。
    ///
    /// 不设 `self.editing`——那个标记只管高度场碰撞体要不要重建，
    /// 贴图混合权重不影响碰撞形状。
    fn paint_texture(&mut self, ctx: &mut Context) {
        let Some(point) = self.brush_target(ctx) else {
            return;
        };
        let to_local = ctx.scene.world_matrix(self.terrain).inverse();
        let local = to_local.transform_point3(point);
        let brush = Brush {
            center: Vec2::new(local.x, local.z),
            radius: self.radius,
            strength: 0.5,
            falloff: 0.6,
        };

        if let Some(terrain) = ctx
            .scene
            .try_get_mut(self.terrain)
            .and_then(Node::terrain_mut)
        {
            // `SplatMap::paint` 要一份 `&Heightmap` 只是为了知道笔刷覆盖
            // 哪些顶点，跟高度值本身无关；克隆一份避免同时借用
            // `heightmap()` 和 `splat_mut()`。
            let heightmap = terrain.heightmap().clone();
            terrain
                .splat_mut()
                .paint(&heightmap, &brush, self.paint_layer);
        }
    }

    /// 造一张地表贴图：底色上叠一层廉价的按像素哈希噪声，纯色会让
    /// 混合边界在画面上完全看不出来，加一点粗糙的纹理细节才看得出
    /// 「这一块换成另一层了」。
    fn layer_texture(base: [u8; 3]) -> Texture {
        const SIZE: u32 = 64;
        let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                // 没有引入随机数依赖：位运算哈希足够让人眼看出「有纹理」，
                // 不需要真的均匀分布。
                let hash = (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263)) & 0xff;
                let shade = 0.75 + (hash as f32 / 255.0) * 0.35;
                for channel in base {
                    pixels.push((channel as f32 * shade).min(255.0) as u8);
                }
                pixels.push(255);
            }
        }
        // 默认采样器就是线性过滤 + 平铺，地形贴图正好要这个，不必再改。
        Texture::new(SIZE, SIZE, pixels)
    }

    /// splat 材质：四层地表贴图叠成一个纹理数组，混合权重来自
    /// [`SplatMap::to_texture`]（由 `Scene::update` 在笔刷涂改后自动重传，
    /// 见模块文档）。这里只需要建一次纹理数组、挂上着色器。
    fn splat_material() -> Material {
        let layers = Texture::from_layers(&[
            Self::layer_texture([86, 138, 62]),   // 草
            Self::layer_texture([110, 108, 104]), // 岩
            Self::layer_texture([120, 84, 54]),   // 土
            Self::layer_texture([225, 230, 235]), // 雪
        ]);
        Material::standard()
            .with_shader(Resource::new_ok(
                "terrain_splat.wgsl",
                Shader::snippet(include_str!("terrain_splat.wgsl")),
            ))
            .with_texture_array(Resource::new_ok("terrain_layers", layers))
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
                .with_terrain(Terrain::new(Self::make_heightmap(), 32, LAYERS))
                .with_material(Self::splat_material()),
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
        klog::info!(
            "WASD/QE 移动，左键抬升、右键下压、中键抹平，1~4 选贴图层、Shift+左键涂层，[ ] 改笔刷，H 看分块，空格丢球"
        );
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

        // 选贴图层：数字键对应权重图的 R/G/B/A 四个通道。
        for (key, layer) in [
            (KeyCode::Digit1, 0),
            (KeyCode::Digit2, 1),
            (KeyCode::Digit3, 2),
            (KeyCode::Digit4, 3),
        ] {
            if ctx.input.key_just_pressed(key) {
                self.paint_layer = layer;
                klog::info!("贴图层 → {}", layer + 1);
            }
        }

        // 笔刷。Shift 把左键从「抬升」换成「涂贴图层」，必须排在
        // 普通左键判断之前，否则两条分支永远只有第一条生效。
        if ctx.input.mouse_pressed(MouseButton::Left) && ctx.input.key_pressed(KeyCode::ShiftLeft) {
            self.paint_texture(ctx);
        } else if ctx.input.mouse_pressed(MouseButton::Left) {
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
