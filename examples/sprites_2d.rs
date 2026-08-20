//! 2D：图集打包、瓦片地图、批处理精灵、九宫格。
//!
//! ```bash
//! cargo run --example sprites_2d
//! ```
//!
//! WASD 移动相机，`[` `]` 改精灵数量，H 看统计。
//!
//! # 两条 2D 路径
//!
//! - **挂在节点上的 `Sprite`**：走 3D 管线（一个贴了图的方片）。
//!   享有剔除、光照、阴影，但逐精灵提交一次。
//! - **`scene.push_sprite`**：走专用的 2D 批处理——排序、合批、实例化。
//!   同一张纹理的相邻精灵一次画完。这个例子用的是后者。
//!
//! 几百个精灵两者都行；几万个时差别很大。
//!
//! # 顺序必须由 CPU 定
//!
//! 精灵全在同一个平面上，深度值一样，深度缓冲帮不上忙。
//! 引擎按「层 → 层内 Y → 纹理」三级排序，越靠下的越晚画。

use kengine::ksprite::{PackEntry, PackOptions, SpriteRegion, TileMap, nine_slice, pack};
use kengine::prelude::*;

/// 每格多大（世界单位）。
const TILE: f32 = 1.0;
/// 地图尺寸（格）。
const MAP_W: usize = 60;
const MAP_H: usize = 40;

#[derive(Default)]
struct Sprites2d {
    camera: Handle<Node>,
    /// 打包好的图集贴图 id。
    texture: Option<kengine::kcore::uuid::Uuid>,
    atlas: Atlas,
    /// 会动的精灵。
    movers: Vec<Mover>,
    count: usize,
    elapsed: f32,
    show_stats: bool,
}

/// 一个绕圈跑的精灵。
struct Mover {
    center: Vec2,
    radius: f32,
    speed: f32,
    phase: f32,
    region: SpriteRegion,
    color: Vec4,
}

/// 一个无光照的贴图材质。
///
/// 把基础色压黑、亮度全放进自发光——于是它在现有的 PBR 管线里
/// 表现为无光照的贴图，不必为 2D 另开一条渲染路径。
fn unlit(texture: &Resource<Texture>) -> Material {
    Material::standard()
        .with_base_color(Vec4::new(0.0, 0.0, 0.0, 1.0))
        .with_base_color_texture(texture.clone())
        .with(kengine::kpbr::standard::EMISSIVE, Vec3::ONE)
        .with(kengine::kpbr::standard::EMISSIVE_TEXTURE, texture.clone())
}

impl Sprites2d {
    /// 造几张纯色小图，打包成一张图集。
    ///
    /// 真实项目里这些是磁盘上的 PNG；这里程序化生成，免得往仓库里
    /// 塞美术资源。
    fn build_atlas() -> (Texture, Atlas) {
        let tile = |w: u32, h: u32, rgba: [u8; 4]| {
            let mut data = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    // 边上描一圈深色，好看出每一格的边界与 UV 对不对。
                    let edge = x == 0 || y == 0 || x == w - 1 || y == h - 1;
                    let c = if edge {
                        [rgba[0] / 3, rgba[1] / 3, rgba[2] / 3, 255]
                    } else {
                        rgba
                    };
                    data.extend_from_slice(&c);
                }
            }
            Texture::new(w, h, data)
        };

        // 故意用几种不同的尺寸——`Atlas::grid` 只能处理等大的格子，
        // 打包器存在的意义就是应付这种情况。
        let entries = vec![
            PackEntry::new("grass", tile(16, 16, [90, 150, 70, 255])),
            PackEntry::new("dirt", tile(16, 16, [140, 110, 70, 255])),
            PackEntry::new("water", tile(16, 16, [60, 110, 190, 255])),
            PackEntry::new("stone", tile(16, 16, [120, 120, 130, 255])),
            PackEntry::new("coin", tile(12, 12, [230, 200, 60, 255])),
            PackEntry::new("gem", tile(10, 14, [200, 80, 200, 255])),
            PackEntry::new("panel", tile(24, 24, [40, 45, 60, 255])),
        ];

        let packed = pack(
            &entries,
            PackOptions {
                size: 256,
                padding: 2,
                extrude: true,
            },
        );
        if !packed.rejected.is_empty() {
            klog::warn!("这些图没塞进图集：{:?}", packed.rejected);
        }
        (packed.texture, packed.atlas)
    }

    /// 造一张地图。
    fn build_map(&self) -> TileMap {
        let mut map = TileMap::new(MAP_W, MAP_H, Vec2::splat(TILE));
        // 瓦片编号是图集里的**下标**，所以要按打包后的顺序查。
        let index_of =
            |name: &str| (0..self.atlas.len()).find(|i| self.atlas.name(*i) == Some(name));
        let grass = index_of("grass").unwrap_or(0) as u32;
        let dirt = index_of("dirt").unwrap_or(0) as u32;
        let water = index_of("water").unwrap_or(0) as u32;

        for row in 0..MAP_H {
            for col in 0..MAP_W {
                let x = col as f32 / MAP_W as f32;
                let y = row as f32 / MAP_H as f32;
                let n = (x * 9.0).sin() + (y * 7.0).cos();
                map.set(
                    col,
                    row,
                    if n < -0.9 {
                        water
                    } else if n < 0.2 {
                        dirt
                    } else {
                        grass
                    },
                );
            }
        }
        map
    }

    /// 重新生成会动的精灵。
    fn respawn(&mut self) {
        let coin = self.atlas.find("coin").unwrap_or(SpriteRegion::FULL);
        let gem = self.atlas.find("gem").unwrap_or(SpriteRegion::FULL);

        self.movers = (0..self.count)
            .map(|i| {
                let t = i as f32;
                Mover {
                    center: Vec2::new(
                        (t * 0.37).sin() * 0.5 * MAP_W as f32 + MAP_W as f32 * 0.5,
                        (t * 0.53).cos() * 0.5 * MAP_H as f32 + MAP_H as f32 * 0.5,
                    ),
                    radius: 1.0 + (t * 0.11).sin().abs() * 4.0,
                    speed: 0.4 + (t * 0.07).sin().abs() * 1.2,
                    phase: t * 0.9,
                    region: if i % 3 == 0 { gem } else { coin },
                    color: Vec4::new(1.0, 1.0, 1.0, 1.0),
                }
            })
            .collect();
    }
}

impl Plugin for Sprites2d {
    fn init(&mut self, ctx: &mut Context) {
        let b = ctx.input.bindings_mut();
        b.bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
        b.bind_axis("vertical", KeyCode::KeyW, KeyCode::KeyS);
        b.bind_action("fewer", KeyCode::BracketLeft);
        b.bind_action("more", KeyCode::BracketRight);
        b.bind_action("stats", KeyCode::KeyH);

        let (texture, atlas) = Self::build_atlas();
        self.texture = Some(texture.id());
        self.atlas = atlas;

        // 同一张图走两条路：
        // - 登记成资源，给走 3D 管线的地图和面板当材质贴图；
        // - 登记进场景，给专用的 2D 精灵管线用（它只认纹理 id）。
        //
        // 两边必须是**同一个 id**，否则精灵那一批会被跳过。
        // `Texture::clone` 保留 id，所以克隆是安全的。
        ctx.scene.register_sprite_texture(texture.clone());
        let texture = ctx.resources.register("builtin/atlas2d", texture);

        // 正交相机，看向 -Z。2D 铺在 XY 平面上。
        self.camera = ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera {
                    projection: Projection::Orthographic { height: 30.0 },
                    ..Default::default()
                })
                .with_position(Vec3::new(MAP_W as f32 * 0.5, MAP_H as f32 * 0.5, 10.0)),
        );

        // 地图合成一块网格，挂成一个普通节点走 3D 管线——
        // 它是静态的，不需要每帧排序。
        let map = self.build_map();
        let mesh = map.build(&self.atlas, None);
        klog::info!(
            "地图 {MAP_W}×{MAP_H} = {} 格，合成 {} 个三角形，一次绘制",
            MAP_W * MAP_H,
            mesh.indices().len() / 3
        );
        ctx.scene.add_node(
            Node::new("TileMap")
                .with_mesh(mesh)
                .with_material(unlit(&texture))
                .with_position(Vec3::new(0.0, 0.0, -1.0)),
        );

        // 九宫格：一张 24×24 的小图拉成一块大面板。
        if let Some(region) = self.atlas.find("panel") {
            let panel = nine_slice::build(
                (Vec2::new(1.0, 1.0), Vec2::new(18.0, 6.0)),
                region,
                Vec2::splat(256.0),
                Slices::all(8.0),
                Vec4::new(1.0, 1.0, 1.0, 0.9),
            );
            ctx.scene.add_node(
                Node::new("Panel")
                    .with_mesh(panel)
                    .with_material(unlit(&texture))
                    .with_position(Vec3::new(0.0, 0.0, 1.0)),
            );
        }

        self.count = 2000;
        self.respawn();
        klog::info!("WASD 移动，[ ] 改精灵数（当前 {}），H 看统计", self.count);
    }

    fn update(&mut self, ctx: &mut Context) {
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
        self.elapsed += ctx.dt;

        if let Some(node) = ctx.scene.try_get_mut(self.camera) {
            let speed = 15.0 * ctx.dt;
            node.transform.position += Vec3::new(
                ctx.input.axis("horizontal") * speed,
                ctx.input.axis("vertical") * speed,
                0.0,
            );
        }

        if ctx.input.action_just_pressed("more") {
            self.count = (self.count * 2).min(200_000);
            self.respawn();
            klog::info!("精灵数：{}", self.count);
        }
        if ctx.input.action_just_pressed("fewer") {
            self.count = (self.count / 2).max(1);
            self.respawn();
            klog::info!("精灵数：{}", self.count);
        }
        if ctx.input.action_just_pressed("stats") {
            self.show_stats = !self.show_stats;
        }

        let Some(texture) = self.texture else {
            return;
        };

        // 每帧重新提交。即时模式——不提交就不画。
        for mover in &self.movers {
            let angle = self.elapsed * mover.speed + mover.phase;
            let position = mover.center + Vec2::new(angle.cos(), angle.sin()) * mover.radius;
            ctx.scene.push_sprite(
                SpriteInstance::new(position, Vec2::splat(0.6), texture)
                    .with_region(mover.region)
                    .with_color(mover.color)
                    // 层留给设计用；层内按 Y 排，越靠下的越晚画。
                    .with_layer(1)
                    .with_rotation(angle * 0.5),
            );
        }

        if self.show_stats {
            klog::info!(
                "精灵 {} / 绘制调用 {} / 三角形 {}",
                ctx.stats.sprites,
                ctx.stats.draw_calls,
                ctx.stats.triangles,
            );
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — 2D sprites")
        .add_plugin(Sprites2d::default())
        .run();
}
