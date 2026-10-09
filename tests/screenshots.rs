//! 截图回归：几个覆盖面广的场景用无头渲染器画出来，和 `tests/golden/*.png` 比。
//!
//! ```bash
//! cargo test --test screenshots -- --ignored              # 比对
//! KENGINE_BLESS=1 cargo test --test screenshots -- --ignored  # 刷新基准图
//! ```
//!
//! 默认 `#[ignore]`：基准图是在某一块显卡上画的，换显卡 / 驱动会有细微差别，
//! 不该让别人的 `cargo test` 因此变红。改渲染器之前跑一遍、改完再跑一遍，
//! 这才是它的用法。没有显卡适配器的机器上整组跳过。
//!
//! # 怎么比
//!
//! 逐像素比太脆（PCF、抗锯齿边缘差一两级就红），整体均值又太钝（一个物体的影子没了，
//! 全图均值几乎不动）。这里按 8×8 分块取平均色再比：
//!
//! - 任何一块差超过 `BLOCK_LIMIT`（0–255）→ 失败。抓「某处东西没了 / 颜色错了」。
//! - 全图平均差超过 `MEAN_LIMIT` → 失败。抓「整体偏暗 / 偏色」。
//!
//! 失败时把实际画面和放大过的差异图写到 `target/screenshots/`，消息里给出路径。
//! 比对的代码在 `tests/common/mod.rs`，例子级截图（`tests/example_screenshots.rs`）共用。

use kengine::krender::Renderer;
use kengine::prelude::*;

mod common;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 160;
/// 画几帧再截：头一帧要建管线、传网格，时间性效果也要几帧才稳定。
const FRAMES: usize = 4;

fn camera(scene: &mut Scene, eye: Vec3, target: Vec3) {
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default())
            .with_transform(Transform::looking_at(eye, target, Vec3::Y)),
    );
}

fn ground(scene: &mut Scene) {
    ambient(scene);
    scene.add_node(
        Node::new("Ground")
            .with_mesh(Mesh::plane(1.0))
            .with_material(Material::default().with_base_color(Vec4::new(0.6, 0.6, 0.62, 1.0)))
            .with_scale(Vec3::new(12.0, 1.0, 12.0)),
    );
}

/// 暗一点的环境光：不加的话默认环境光很亮，影子和灯光的对比被冲淡，回归抓不到。
fn ambient(scene: &mut Scene) {
    scene.add_node(Node::new("Ambient").with_light(
        Light::hemisphere(Vec3::new(0.12, 0.12, 0.14)).with_color(Vec3::new(0.32, 0.36, 0.44)),
    ));
}

fn sun(scene: &mut Scene, shadows: bool) {
    let light = Light::directional().with_intensity(2.0);
    let light = if shadows { light.with_shadows() } else { light };
    scene.add_node(
        Node::new("Sun")
            .with_light(light)
            .with_transform(Transform::looking_at(
                Vec3::new(-3.0, 5.0, -1.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
}

/// 用一个场景画 `FRAMES` 帧，读回像素，和基准图比。
fn check(name: &str, scene: &Scene, draw_ui: impl Fn(&mut Ui)) {
    let Some(mut renderer) = pollster::block_on(Renderer::headless(WIDTH, HEIGHT)) else {
        eprintln!("没有可用的显卡适配器，跳过 {name}");
        return;
    };
    // 场景都只有几米大：默认一百多米的阴影距离会把级联拉得很粗，影子糊成一片。
    renderer.set_shadow_cascades(kengine::krender::CascadeSettings {
        max_distance: 15.0,
        ..Default::default()
    });
    let mut ui = Ui::new();
    for _ in 0..FRAMES {
        ui.begin_frame(Vec2::new(WIDTH as f32, HEIGHT as f32), 1.0);
        draw_ui(&mut ui);
        ui.end_frame();
        renderer.render(scene, &ui, &[]);
    }
    let (width, height, actual) = renderer.read_pixels().expect("读回像素");

    if let Err(message) = common::verify(name, width, height, &actual) {
        panic!("{message}");
    }
}

/// 方向光 + 级联阴影：方块的影子落在地面上。
#[test]
#[ignore]
fn shadow_cube() {
    let mut scene = Scene::new();
    camera(
        &mut scene,
        Vec3::new(4.0, 3.5, 5.0),
        Vec3::new(0.0, 0.5, 0.0),
    );
    ground(&mut scene);
    sun(&mut scene, true);
    scene.add_node(
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_material(Material::default().with_base_color(Vec4::new(0.8, 0.3, 0.2, 1.0)))
            .with_position(Vec3::new(0.0, 0.5, 0.0)),
    );
    scene.update();
    check("shadow_cube", &scene, |_| {});
}

/// 一排球：粗糙度从左到右变大，上排金属、下排非金属。
#[test]
#[ignore]
fn pbr_spheres() {
    let mut scene = Scene::new();
    camera(&mut scene, Vec3::new(0.0, 0.0, 9.0), Vec3::ZERO);
    sun(&mut scene, false);
    scene.add_node(Node::new("Sky").with_light(Light::hemisphere(Vec3::new(0.15, 0.12, 0.1))));
    let sphere = Mesh::sphere(24, 32);
    for row in 0..2 {
        for column in 0..5 {
            let material = Material::default()
                .with_base_color(Vec4::new(0.9, 0.7, 0.3, 1.0))
                .with_metallic(if row == 0 { 1.0 } else { 0.0 })
                .with_roughness(0.1 + column as f32 * 0.2);
            scene.add_node(
                Node::new("Sphere")
                    .with_mesh(sphere.clone())
                    .with_material(material)
                    .with_position(Vec3::new(
                        column as f32 * 1.6 - 3.2,
                        0.9 - row as f32 * 1.8,
                        0.0,
                    ))
                    .with_scale(Vec3::splat(0.7)),
            );
        }
    }
    scene.update();
    check("pbr_spheres", &scene, |_| {});
}

/// 点光 + 聚光，各自带颜色：聚簇着色那条路。
#[test]
#[ignore]
fn point_and_spot_lights() {
    let mut scene = Scene::new();
    camera(&mut scene, Vec3::new(0.0, 4.0, 7.0), Vec3::ZERO);
    ground(&mut scene);
    scene.add_node(
        Node::new("Red")
            .with_light(
                Light::point(6.0)
                    .with_color(Vec3::new(1.0, 0.2, 0.1))
                    .with_intensity(8.0),
            )
            .with_position(Vec3::new(-2.0, 1.0, 0.0)),
    );
    scene.add_node(
        Node::new("Spot")
            .with_light(
                Light::spot(10.0, 18.0, 28.0)
                    .with_color(Vec3::new(0.2, 0.5, 1.0))
                    .with_intensity(80.0),
            )
            .with_transform(Transform::looking_at(
                Vec3::new(2.0, 4.0, 1.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::Z,
            )),
    );
    scene.add_node(
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_position(Vec3::new(0.0, 0.5, 0.0)),
    );
    scene.update();
    check("point_and_spot_lights", &scene, |_| {});
}

/// 半透明：两块互相叠着的玻璃板，后面一个不透明方块。
#[test]
#[ignore]
fn transparency() {
    let mut scene = Scene::new();
    camera(&mut scene, Vec3::new(0.0, 1.0, 6.0), Vec3::ZERO);
    sun(&mut scene, false);
    ambient(&mut scene);
    scene.add_node(
        Node::new("Back")
            .with_mesh(Mesh::cube())
            .with_position(Vec3::new(0.0, 0.0, -2.0)),
    );
    for (index, color) in [Vec4::new(1.0, 0.2, 0.2, 0.5), Vec4::new(0.2, 0.4, 1.0, 0.5)]
        .into_iter()
        .enumerate()
    {
        scene.add_node(
            Node::new("Glass")
                .with_mesh(Mesh::cube())
                .with_material(
                    Material::default()
                        .with_base_color(color)
                        .with_blend_mode(kengine::kmaterial::BlendMode::Alpha),
                )
                .with_position(Vec3::new(index as f32 * 0.8 - 0.4, 0.0, index as f32 * 0.5))
                .with_scale(Vec3::new(1.2, 1.2, 0.1)),
        );
    }
    scene.update();
    check("transparency", &scene, |_| {});
}

/// UI 叠在 3D 画面上（不带文字：系统字体因机器而异）。
#[test]
#[ignore]
fn ui_overlay() {
    let mut scene = Scene::new();
    camera(&mut scene, Vec3::new(3.0, 3.0, 4.0), Vec3::ZERO);
    ground(&mut scene);
    sun(&mut scene, true);
    scene.add_node(
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_position(Vec3::new(0.0, 0.5, 0.0)),
    );
    scene.update();
    check("ui_overlay", &scene, |ui| {
        let panel = UiRect {
            min: Vec2::new(10.0, 10.0),
            max: Vec2::new(120.0, 70.0),
        };
        ui.rounded_rect(panel, 8.0, Vec4::new(0.02, 0.02, 0.03, 0.85));
        ui.border(panel, 8.0, 1.0, Vec4::new(1.0, 1.0, 1.0, 0.2));
        ui.rect(
            UiRect {
                min: Vec2::new(20.0, 30.0),
                max: Vec2::new(90.0, 38.0),
            },
            Vec4::new(0.05, 0.3, 1.0, 1.0),
        );
        ui.polyline(
            &[
                Vec2::new(20.0, 60.0),
                Vec2::new(50.0, 45.0),
                Vec2::new(80.0, 55.0),
                Vec2::new(110.0, 42.0),
            ],
            2.0,
            Vec4::new(1.0, 0.6, 0.1, 1.0),
        );
    });
}

/// 覆盖层相机：主相机眼前一堵墙，覆盖层相机看见的方块（另一个渲染层）照样叠在上面——
/// 不和主画面比深度。覆盖层用自己的 FOV，墙不出现在覆盖层里。
#[test]
#[ignore]
fn overlay_camera() {
    let mut scene = Scene::new();
    ambient(&mut scene);
    sun(&mut scene, false);
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default().with_layers(1))
            .with_transform(Transform::looking_at(
                Vec3::new(0.0, 0.0, 3.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    scene.add_node(
        Node::new("Wall")
            .with_mesh(Mesh::cube())
            .with_material(Material::default().with_base_color(Vec4::new(0.3, 0.5, 0.3, 1.0)))
            .with_position(Vec3::new(0.0, 0.0, 2.0))
            .with_scale(Vec3::new(6.0, 6.0, 0.2)),
    );
    // 覆盖层相机放在别处，只看第 2 层。
    scene.add_node(
        Node::new("Overlay")
            .with_camera(
                Camera::perspective(40.0)
                    .with_layers(2)
                    .with_target(CameraTarget::Overlay),
            )
            .with_transform(Transform::looking_at(
                Vec3::new(100.0, 0.0, 3.0),
                Vec3::new(100.0, 0.0, 0.0),
                Vec3::Y,
            )),
    );
    scene.add_node(
        Node::new("Held")
            .with_mesh(Mesh::sphere(16, 24))
            .with_material(Material::default().with_base_color(Vec4::new(0.9, 0.6, 0.1, 1.0)))
            .with_render_layers(2)
            .with_position(Vec3::new(100.6, -0.4, 0.0))
            .with_scale(Vec3::splat(0.6)),
    );
    scene.update();
    check("overlay_camera", &scene, |_| {});
}

/// 多光源阴影：太阳是主投影光源（级联），另外一盏聚光和一盏点光也投影——
/// 原来一帧只有第一盏投影的灯有影子，这两盏的影子都会丢。
#[test]
#[ignore]
fn local_light_shadows() {
    let mut scene = Scene::new();
    camera(
        &mut scene,
        Vec3::new(0.0, 7.0, 7.0),
        Vec3::new(0.0, 0.0, 0.5),
    );
    ground(&mut scene);
    scene.add_node(
        Node::new("Sun")
            .with_light(Light::directional().with_intensity(0.3).with_shadows())
            .with_transform(Transform::looking_at(
                Vec3::new(-3.0, 5.0, -1.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    scene.add_node(
        Node::new("Spot")
            .with_light(
                Light::spot(12.0, 25.0, 35.0)
                    .with_intensity(60.0)
                    .with_shadows(),
            )
            .with_transform(Transform::looking_at(
                Vec3::new(-2.5, 4.0, 0.0),
                Vec3::new(-2.5, 0.0, 0.0),
                Vec3::Z,
            )),
    );
    scene.add_node(
        Node::new("Point")
            .with_light(
                Light::point(8.0)
                    .with_color(Vec3::new(1.0, 0.8, 0.5))
                    .with_intensity(25.0)
                    .with_shadows(),
            )
            .with_position(Vec3::new(2.5, 2.5, 0.0)),
    );
    for x in [-2.5, 2.5] {
        scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(Material::default().with_base_color(Vec4::new(0.7, 0.7, 0.75, 1.0)))
                .with_position(Vec3::new(x + 0.4, 0.5, 0.6))
                .with_scale(Vec3::splat(0.8)),
        );
    }
    scene.update();
    check("local_light_shadows", &scene, |_| {});
}
