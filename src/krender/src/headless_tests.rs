//! 要真显卡的测试：起一个无头渲染器画几帧。机器上没有适配器（CI 常见）时直接跳过。

use crate::Renderer;
use kcamera::Camera;
use kmath::{Vec2, Vec3};
use kmesh::Mesh;
use kscene::{Node, Scene, Transform};
use kui::Ui;

fn renderer() -> Option<Renderer> {
    let renderer = pollster::block_on(Renderer::headless(64, 64));
    if renderer.is_none() {
        eprintln!("没有可用的显卡适配器，跳过");
    }
    renderer
}

fn empty_ui() -> Ui {
    let mut ui = Ui::new();
    ui.begin_frame(Vec2::new(64.0, 64.0), 1.0);
    ui.end_frame();
    ui
}

fn camera(scene: &mut Scene) {
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default())
            .with_transform(Transform::looking_at(
                Vec3::new(0.0, 2.0, 6.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
}

#[test]
fn headless_frames_produce_pixels() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let mut scene = Scene::new();
    camera(&mut scene);
    scene.add_node(Node::new("Cube").with_mesh(Mesh::cube()));
    scene.update();
    let ui = empty_ui();
    for _ in 0..2 {
        renderer.render(&scene, &ui, &[]);
    }
    let (width, height, pixels) = renderer.read_pixels().expect("无头渲染器读得回像素");
    assert_eq!((width, height), (64, 64));
    assert_eq!(pixels.len(), 64 * 64 * 4);
    // 画面中心是方块，四角是天空：两处颜色不该一样。
    let at = |x: usize, y: usize| &pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
    assert_ne!(at(32, 32), at(1, 1), "中心和角落一样，方块没画上");
}

#[test]
fn unused_meshes_are_evicted() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    renderer.set_eviction_frames(10);
    let mut scene = Scene::new();
    camera(&mut scene);
    // 两份不同的网格（各自一个 id），各占一份显存。
    scene.add_node(Node::new("A").with_mesh(Mesh::cube()));
    let b = scene.add_node(
        Node::new("B")
            .with_mesh(Mesh::sphere(8, 8))
            .with_position(Vec3::new(1.5, 0.0, 0.0)),
    );
    scene.update();
    let ui = empty_ui();
    for _ in 0..3 {
        renderer.render(&scene, &ui, &[]);
    }
    // 统计是在一帧开头记的，看到的是上一帧留下的数。
    assert_eq!(renderer.stats().gpu_meshes, 2);

    scene.remove_node(b);
    scene.update();
    for _ in 0..80 {
        renderer.render(&scene, &ui, &[]);
    }
    assert_eq!(renderer.stats().gpu_meshes, 1, "删掉的节点的网格应该被回收");
}

/// 画一帧带影子的场景：地面 + 斜照的方向光 + 一个方块，读回像素。
fn shadow_scene_pixels(cube: Node, shadows: bool) -> Option<Vec<u8>> {
    shadow_scene_pixels_with(cube, shadows, 0.0)
}

fn shadow_scene_pixels_with(cube: Node, shadows: bool, radius: f32) -> Option<Vec<u8>> {
    use klight::Light;
    use kmaterial::Material;
    use kmath::Vec4;
    let mut renderer = pollster::block_on(Renderer::headless(96, 96))?;
    renderer.set_shadow_cascades(klight::cascade::CascadeSettings {
        max_distance: 20.0,
        ..Default::default()
    });
    let mut scene = Scene::new();
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default())
            .with_transform(Transform::looking_at(
                Vec3::new(1.5, 9.0, 6.0),
                Vec3::new(1.5, 0.0, 0.0),
                Vec3::Y,
            )),
    );
    scene.add_node(
        Node::new("Ground")
            .with_mesh(Mesh::plane(1.0))
            .with_material(Material::default().with_base_color(Vec4::new(0.7, 0.7, 0.7, 1.0)))
            .with_scale(Vec3::new(14.0, 1.0, 14.0)),
    );
    let sun = Light::directional()
        .with_intensity(2.5)
        .with_shadow_radius(radius);
    scene.add_node(
        Node::new("Sun")
            .with_light(if shadows { sun.with_shadows() } else { sun })
            .with_transform(Transform::looking_at(
                Vec3::new(-4.0, 5.0, -1.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    scene.add_node(cube);
    scene.update();
    let mut ui = Ui::new();
    for _ in 0..3 {
        ui.begin_frame(Vec2::new(96.0, 96.0), 1.0);
        ui.end_frame();
        renderer.render(&scene, &ui, &[]);
    }
    renderer.read_pixels().map(|(_, _, pixels)| pixels)
}

/// 明显不同（某个通道差超过 16 级）的像素数。影子只占画面一小块，按全图平均差会被冲淡。
fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4)
        .zip(b.chunks(4))
        .filter(|(x, y)| {
            x.iter()
                .zip(y.iter())
                .take(3)
                .any(|(p, q)| p.abs_diff(*q) > 16)
        })
        .count()
}

#[test]
fn a_vertex_hook_moves_the_shadow_too() {
    use kasset::Resource;
    use kmaterial::Material;
    use kmath::Vec4;
    use kshader::Shader;
    // 顶点钩子把方块往 +x 挪 3 米。阴影 pass 也得过这个钩子，否则影子还落在原点那儿。
    let hook = Shader::snippet(
        "fn material_vertex(vertex: VertexSurface) -> VertexSurface {\n    var out = vertex;\n    out.position.x += vertex.params[0].x;\n    return out;\n}",
    );
    let color = Vec4::new(0.8, 0.3, 0.2, 1.0);
    let hooked = Node::new("Hooked")
        .with_mesh(Mesh::cube())
        .with_material(
            Material::default()
                .with_base_color(color)
                .with_shader(Resource::new_ok("shift hook", hook))
                .with_param(0, 3.0f32),
        )
        .with_position(Vec3::new(0.0, 0.5, 0.0));
    // 对照：普通方块直接摆在 x = 3。
    let placed = || {
        Node::new("Placed")
            .with_mesh(Mesh::cube())
            .with_material(Material::default().with_base_color(color))
            .with_position(Vec3::new(3.0, 0.5, 0.0))
    };
    let Some(hooked) = shadow_scene_pixels(hooked, true) else {
        eprintln!("没有可用的显卡适配器，跳过");
        return;
    };
    let placed_with_shadow = shadow_scene_pixels(placed(), true).unwrap();
    let placed_without_shadow = shadow_scene_pixels(placed(), false).unwrap();
    // 先确认这个机位下影子看得见：开不开阴影两张图差得明显。
    let shadow_visible = changed_pixels(&placed_with_shadow, &placed_without_shadow);
    assert!(
        shadow_visible > 40,
        "对照场景里影子只占 {shadow_visible} 个像素，这个测试测不出东西"
    );
    // 钩子挪过去的方块和直接摆过去的方块：方块、影子都应该在同一个地方。
    let difference = changed_pixels(&hooked, &placed_with_shadow);
    assert!(
        difference * 10 < shadow_visible,
        "带顶点钩子的方块的影子没跟着挪：和对照有 {difference} 个像素不同（影子本身占 {shadow_visible} 个）"
    );
}

#[test]
fn a_cutout_hook_cuts_the_shadow_too() {
    use kasset::Resource;
    use kmaterial::Material;
    use kmath::Vec4;
    use kshader::Shader;
    // 表面钩子把整个方块 discard 掉：主画面里看不见它，阴影里也不该有它。
    let hook = Shader::snippet(
        "fn material_surface(s: Surface) -> Surface {\n    if (s.params[0].x > 0.5) { discard; }\n    return s;\n}",
    );
    let cube = |hooked: bool| {
        let material = Material::default().with_base_color(Vec4::new(0.8, 0.3, 0.2, 1.0));
        let material = if hooked {
            material
                .with_shader(Resource::new_ok("cutout hook", hook.clone()))
                .with_param(0, 1.0f32)
        } else {
            material
        };
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_material(material)
            .with_position(Vec3::new(3.0, 0.5, 0.0))
    };
    let Some(cut) = shadow_scene_pixels(cube(true), true) else {
        eprintln!("没有可用的显卡适配器，跳过");
        return;
    };
    let solid = shadow_scene_pixels(cube(false), true).unwrap();
    // 对照：一个没有画面贡献的空节点，就是「没有这个方块」。
    let empty = shadow_scene_pixels(
        Node::new("Nothing").with_position(Vec3::new(3.0, 0.5, 0.0)),
        true,
    )
    .unwrap();
    let cube_and_shadow = changed_pixels(&solid, &empty);
    assert!(
        cube_and_shadow > 40,
        "对照场景里方块和影子只占 {cube_and_shadow} 个像素"
    );
    let leftover = changed_pixels(&cut, &empty);
    assert!(
        leftover * 10 < cube_and_shadow,
        "被 discard 掉的方块还留着影子：和空场景有 {leftover} 个像素不同（方块 + 影子占 {cube_and_shadow} 个）"
    );
}

#[test]
fn a_shadow_radius_softens_the_edge() {
    use kmaterial::Material;
    use kmath::Vec4;
    let cube = || {
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_material(Material::default().with_base_color(Vec4::new(0.8, 0.3, 0.2, 1.0)))
            .with_position(Vec3::new(3.0, 0.5, 0.0))
    };
    let Some(hard) = shadow_scene_pixels_with(cube(), true, 0.0) else {
        eprintln!("没有可用的显卡适配器，跳过");
        return;
    };
    let soft = shadow_scene_pixels_with(cube(), true, 16.0).unwrap();
    let lit = shadow_scene_pixels_with(cube(), false, 0.0).unwrap();
    // 半影：比受光暗、但又没暗到影子最深处那么多的地面像素。按和「不开阴影」那张图的差来分：
    // 差得很少 = 受光，差很多 = 本影，中间 = 半影。
    let penumbra = |image: &[u8]| {
        let deepest = image
            .chunks(4)
            .zip(lit.chunks(4))
            .map(|(a, b)| b[1].saturating_sub(a[1]))
            .max()
            .unwrap_or(0);
        image
            .chunks(4)
            .zip(lit.chunks(4))
            .filter(|(a, b)| {
                let d = b[1].saturating_sub(a[1]);
                d > deepest / 6 && d < deepest * 5 / 6
            })
            .count()
    };
    let (hard_edge, soft_edge) = (penumbra(&hard), penumbra(&soft));
    assert!(
        soft_edge > hard_edge * 2,
        "阴影半径没让边缘变软：硬 {hard_edge} 个半影像素，软 {soft_edge} 个"
    );
}

#[test]
fn a_shadow_catcher_shows_only_the_shadow() {
    use kasset::Resource;
    use klight::Light;
    use kmaterial::{BlendMode, Material};
    use kmath::Vec4;
    use kshader::Shader;
    // 和 kpbr::shadow_catcher 同一段钩子（krender 不依赖 kpbr）。
    let hook = Shader::snippet(
        "fn material_surface(surface: Surface) -> Surface { var out = surface; out.base_color = vec4<f32>(0.0, 0.0, 0.0, 0.0); out.emissive = vec3<f32>(0.0); return out; }
         fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> { (*surface).base_color.a = max((*surface).base_color.a, 1.0 - input.visibility); return vec3<f32>(0.0); }
         fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> { return vec3<f32>(0.0); }",
    );
    let render = |with_cube: bool, with_ground: bool| {
        let mut renderer = pollster::block_on(Renderer::headless(96, 96))?;
        renderer.set_shadow_cascades(klight::cascade::CascadeSettings {
            max_distance: 20.0,
            ..Default::default()
        });
        let mut scene = Scene::new();
        scene.set_background(Some(Vec3::new(0.2, 0.4, 0.8)));
        scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(1.5, 9.0, 6.0),
                    Vec3::new(1.5, 0.0, 0.0),
                    Vec3::Y,
                )),
        );
        if with_ground {
            let material = Material::default()
                .with_shader(Resource::new_ok("catcher", hook.clone()))
                .with_blend_mode(BlendMode::Alpha);
            scene.add_node(
                Node::new("Ground")
                    .with_mesh(Mesh::plane(1.0))
                    .with_material(material)
                    .with_scale(Vec3::new(14.0, 1.0, 14.0)),
            );
        }
        scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(2.5).with_shadows())
                .with_transform(Transform::looking_at(
                    Vec3::new(-4.0, 5.0, -1.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );
        if with_cube {
            // 方块本身挪到画面外（它的影子还落在地上）：只看影子。
            scene.add_node(
                Node::new("Cube")
                    .with_mesh(Mesh::cube())
                    .with_material(Material::default().with_base_color(Vec4::ONE))
                    .with_position(Vec3::new(3.0, 0.5, 0.0)),
            );
        }
        scene.update();
        let mut ui = Ui::new();
        for _ in 0..3 {
            ui.begin_frame(Vec2::new(96.0, 96.0), 1.0);
            ui.end_frame();
            renderer.render(&scene, &ui, &[]);
        }
        renderer.read_pixels().map(|(_, _, pixels)| pixels)
    };
    let Some(background) = render(false, false) else {
        eprintln!("没有可用的显卡适配器，跳过");
        return;
    };
    let catcher_alone = render(false, true).unwrap();
    // 没有影子时，只接影子的地面完全看不见。
    assert!(
        changed_pixels(&catcher_alone, &background) < 4,
        "没有影子的地方地面应该透明"
    );
    let with_shadow = render(true, true).unwrap();
    let cube_only = render(true, false).unwrap();
    // 有方块时：多出来的除了方块本身，就是影子（变暗的像素）。
    let shadow = changed_pixels(&with_shadow, &cube_only);
    assert!(shadow > 40, "影子没画出来：只有 {shadow} 个像素变了");
}

#[test]
fn a_storage_texture_can_be_sampled_by_a_material() {
    use crate::{ComputeBinding, ComputeContext, StorageFormat};
    use kasset::Resource;
    use kmaterial::Material;
    use kshader::Shader;
    let Some(mut renderer) = renderer() else {
        return;
    };
    let gpu = ComputeContext::from_renderer(&renderer);
    let shader = Shader::from_wgsl(
        "@group(0) @binding(0) var image: texture_storage_2d<rgba16float, write>;\n\
         @compute @workgroup_size(8, 8)\n\
         fn main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
             textureStore(image, vec2<i32>(id.xy), vec4<f32>(0.0, 1.0, 0.0, 1.0));\n\
         }",
    )
    .unwrap();
    let pipeline = gpu.create_pipeline(&shader).unwrap();
    let image = gpu.create_storage_texture("green", 8, 8, StorageFormat::Rgba16Float);
    gpu.dispatch_with(&pipeline, &[ComputeBinding::Texture(&image)], [1, 1, 1]);

    // 不受光照影响：直接把采到的颜色当自发光。
    let unlit = Shader::snippet(
        "fn material_surface(s: Surface) -> Surface {\n    var out = s;\n    out.emissive = s.base_color.rgb;\n    out.base_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n    return out;\n}\n\
         fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> { return vec3<f32>(0.0); }\n\
         fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> { return vec3<f32>(0.0); }",
    );
    let material = Material::default()
        .with_shader(Resource::new_ok("unlit", unlit))
        .with_base_color_texture(Resource::new_ok("compute output", image.texture()));
    let mut scene = Scene::new();
    camera(&mut scene);
    scene.add_node(
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_material(material)
            .with_scale(Vec3::splat(2.0)),
    );
    scene.update();
    let ui = empty_ui();
    for _ in 0..2 {
        renderer.render(&scene, &ui, &[]);
    }
    let (_, _, pixels) = renderer.read_pixels().unwrap();
    let center = &pixels[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 3];
    assert!(
        center[1] > 200 && center[0] < 40 && center[2] < 40,
        "方块中心该是计算着色器写的绿色，实际 {center:?}"
    );
}

#[test]
fn the_noise_library_behaves_like_perlin_noise() {
    use crate::{ComputeBinding, ComputeContext};
    use kshader::Shader;
    let Some(gpu) = ComputeContext::shared_headless() else {
        return;
    };
    // 0..256：格点上的值；256..512：细密采样（相邻间距 0.01）；512..768：worley。
    let source = format!(
        "{}\n@group(0) @binding(0) var<storage, read_write> out: array<f32>;\n\
         @compute @workgroup_size(64)\n\
         fn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n\
             let i = id.x;\n\
             if (i < 256u) {{ out[i] = mx_noise_float(vec3<f32>(f32(i % 7u) - 3.0, f32(i / 7u % 5u), f32(i / 35u))); }}\n\
             else if (i < 512u) {{ out[i] = mx_noise_float(vec3<f32>(f32(i - 256u) * 0.01 + 0.123, 0.37, 1.91)); }}\n\
             else if (i < 768u) {{ out[i] = mx_worley_noise_float(vec3<f32>(f32(i - 512u) * 0.05, 0.3, 0.7), 1.0); }}\n\
         }}",
        kshader::noise::WGSL
    );
    let pipeline = gpu
        .create_pipeline(&Shader::from_wgsl(source).unwrap())
        .unwrap();
    let buffer = gpu.create_buffer_zeroed("noise", 768 * 4);
    gpu.dispatch_with(&pipeline, &[ComputeBinding::Buffer(&buffer)], [12, 1, 1]);
    let values: Vec<f32> = gpu
        .read(&buffer)
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    // Perlin 梯度噪声在格点上恒为 0。
    assert!(
        values[..256].iter().all(|v| v.abs() < 1e-5),
        "格点上不为 0：{:?}",
        &values[..16]
    );
    let fine = &values[256..512];
    assert!(fine.iter().all(|v| v.abs() <= 1.05), "超出 [-1, 1]");
    let (min, max) = fine
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    assert!(max - min > 0.3, "几乎是常数：{min}..{max}");
    assert!(
        fine.windows(2).all(|w| (w[0] - w[1]).abs() < 0.05),
        "不连续"
    );
    let worley = &values[512..768];
    assert!(
        worley.iter().all(|v| (0.0..=1.8).contains(v)),
        "worley 越界"
    );
    assert!(worley.iter().any(|v| *v < 0.3) && worley.iter().any(|v| *v > 0.5));
}

#[test]
fn the_output_hook_sees_the_lit_color_and_has_the_last_word() {
    use kasset::Resource;
    use kmaterial::Material;
    use kshader::Shader;
    // 输出钩子把最终颜色的红绿通道对调：受光的红方块显示成绿的。
    let hook = Shader::snippet(
        "fn material_output(surface: ptr<function, Surface>, color: vec4<f32>) -> vec4<f32> {\n    return vec4<f32>(color.g, color.r, color.b, color.a);\n}",
    );
    let render = |hooked: bool| {
        let mut renderer = renderer()?;
        let mut scene = Scene::new();
        camera(&mut scene);
        scene.add_node(
            Node::new("Sun")
                .with_light(klight::Light::directional())
                .with_transform(Transform::looking_at(
                    Vec3::new(1.0, 2.0, 3.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );
        let mut material =
            Material::default().with_base_color(kmath::Vec4::new(1.0, 0.0, 0.0, 1.0));
        if hooked {
            material = material.with_shader(Resource::new_ok("swap", hook.clone()));
        }
        scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(material)
                .with_scale(Vec3::splat(2.0)),
        );
        scene.update();
        let ui = empty_ui();
        for _ in 0..2 {
            renderer.render(&scene, &ui, &[]);
        }
        let (_, _, pixels) = renderer.read_pixels()?;
        Some(pixels[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 3].to_vec())
    };
    let Some(plain) = render(false) else { return };
    let swapped = render(true).unwrap();
    assert!(plain[0] > 3 * plain[1], "对照组该是红的：{plain:?}");
    // 红绿对调（环境光带来的那点绿也跟着换到红通道）。
    assert!(swapped[1] > 3 * swapped[0], "输出钩子没生效：{swapped:?}");
    assert!(
        (i32::from(swapped[1]) - i32::from(plain[0])).abs() <= 3,
        "对调后亮度变了：{plain:?} → {swapped:?}"
    );
}

#[test]
fn an_additive_material_adds_without_hiding_what_is_behind() {
    use kasset::Resource;
    use kmaterial::{BlendMode, Material};
    use kshader::Shader;
    let unlit = Shader::snippet(
        "fn material_surface(s: Surface) -> Surface {\n    var out = s;\n    out.emissive = s.params[0].rgb;\n    out.base_color = vec4<f32>(0.0, 0.0, 0.0, s.params[0].w);\n    return out;\n}\n\
         fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> { return vec3<f32>(0.0); }\n\
         fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> { return vec3<f32>(0.0); }",
    );
    let render = |front: Option<BlendMode>| {
        let mut renderer = renderer()?;
        let mut scene = Scene::new();
        camera(&mut scene);
        let material = |color: kmath::Vec4| {
            Material::default()
                .with_shader(Resource::new_ok("unlit", unlit.clone()))
                .with_param(0, color)
        };
        scene.add_node(
            Node::new("Back")
                .with_mesh(Mesh::cube())
                .with_material(material(kmath::Vec4::new(0.0, 0.25, 0.0, 1.0)))
                .with_scale(Vec3::splat(2.0)),
        );
        if let Some(blend) = front {
            let glow = material(kmath::Vec4::new(0.25, 0.0, 0.0, 0.5)).with_blend_mode(blend);
            scene.add_node(
                Node::new("Front")
                    .with_mesh(Mesh::cube())
                    .with_material(glow)
                    .with_scale(Vec3::splat(0.6))
                    .with_position(Vec3::new(0.0, 1.0, 3.0)),
            );
        }
        scene.update();
        let ui = empty_ui();
        for _ in 0..2 {
            renderer.render(&scene, &ui, &[]);
        }
        let (_, _, pixels) = renderer.read_pixels()?;
        Some(pixels[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 3].to_vec())
    };
    let Some(back) = render(None) else { return };
    let additive = render(Some(BlendMode::Additive)).unwrap();
    let alpha = render(Some(BlendMode::Alpha)).unwrap();
    // 叠加：绿色一点不少，红色加上了 0.25 × 0.5。
    assert!(
        (i32::from(additive[1]) - i32::from(back[1])).abs() <= 2,
        "叠加把后面压暗了：{back:?} → {additive:?}"
    );
    assert!(
        additive[0] > back[0] + 40,
        "叠加没有加上红色：{back:?} → {additive:?}"
    );
    // alpha 混合：绿色被盖掉一半。
    assert!(
        alpha[1] + 20 < back[1],
        "alpha 混合没有盖住后面：{back:?} → {alpha:?}"
    );
}

#[test]
fn a_volume_texture_interpolates_between_layers() {
    use kasset::Resource;
    use kmaterial::Material;
    use kshader::Shader;
    use ktexture::{Sampler, Texture, TextureFormat};
    // 两层：第 0 层红、第 1 层绿。三维采样在 w = 0.5 处应当是两者各半（层间插值）——纹理数组做不到这个。
    let red_green: Vec<u8> = [[255u8, 0, 0, 255], [0, 255, 0, 255]].concat();
    let volume = Texture::volume(1, 1, 2, red_green)
        .with_format(TextureFormat::Linear)
        .with_sampler(Sampler::data());
    let render = |w: f32| {
        let hook = Shader::snippet(format!(
            "fn material_surface(s: Surface) -> Surface {{\n    var out = s;\n    out.emissive = textureSample(custom_texture_3d, base_color_sampler, vec3<f32>(0.5, 0.5, {w:?})).rgb;\n    out.base_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n    return out;\n}}\n\
             fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {{ return vec3<f32>(0.0); }}\n\
             fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {{ return vec3<f32>(0.0); }}"
        ));
        let mut renderer = renderer()?;
        let mut scene = Scene::new();
        camera(&mut scene);
        let material = Material::default()
            .with_shader(Resource::new_ok("volume", hook))
            .with_texture_3d(Resource::new_ok("rg", volume.clone()));
        scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(material)
                .with_scale(Vec3::splat(2.0)),
        );
        scene.update();
        let ui = empty_ui();
        for _ in 0..2 {
            renderer.render(&scene, &ui, &[]);
        }
        let (_, _, pixels) = renderer.read_pixels()?;
        Some(pixels[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 3].to_vec())
    };
    let Some(front) = render(0.25) else { return };
    let middle = render(0.5).unwrap();
    let back = render(0.75).unwrap();
    assert!(
        front[0] > 200 && front[1] < 30,
        "w = 0.25 该是红的：{front:?}"
    );
    assert!(back[1] > 200 && back[0] < 30, "w = 0.75 该是绿的：{back:?}");
    assert!(
        middle[0] > 120 && middle[1] > 120,
        "w = 0.5 该是红绿各半：{middle:?}"
    );
}

#[test]
fn a_blended_material_casts_a_shadow_only_when_asked() {
    use kmaterial::{BlendMode, Material};
    use kmath::Vec4;
    let cube = |casts: bool| {
        let material = Material::default()
            .with_base_color(Vec4::new(0.8, 0.3, 0.2, 0.5))
            .with_blend_mode(BlendMode::Alpha)
            .with_blended_shadows(casts);
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_material(material)
            .with_position(Vec3::new(3.0, 0.5, 0.0))
    };
    let Some(silent) = shadow_scene_pixels(cube(false), true) else {
        return;
    };
    let casting = shadow_scene_pixels(cube(true), true).unwrap();
    let without_shadows = shadow_scene_pixels(cube(true), false).unwrap();
    // 不开：和关掉阴影的画面一样（半透明的方块本身两边都有）。
    let leaked = changed_pixels(&silent, &without_shadows);
    assert!(
        leaked < 5,
        "没开 blended_shadows 也投了影：{leaked} 个像素不同"
    );
    let shadow = changed_pixels(&casting, &without_shadows);
    assert!(
        shadow > 40,
        "开了 blended_shadows 却没有影子：只有 {shadow} 个像素不同"
    );
}

#[test]
fn instances_draw_every_copy_with_its_own_matrix_color_and_data() {
    use kasset::Resource;
    use kmaterial::Material;
    use kscene::Instance;
    use kshader::Shader;
    // 钩子把（已经乘过实例颜色的）基础色 × instance_data.x 当自发光输出，其余全黑：
    // 像素颜色就只由实例的颜色和数据决定，不受光照影响。
    let hook = Shader::snippet(
        "fn material_surface(surface: Surface) -> Surface {\n    var out = surface;\n    out.emissive = surface.base_color.rgb * surface.instance_data.x;\n    out.base_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n    return out;\n}",
    );
    let Some(mut renderer) = renderer() else {
        return;
    };
    let mut scene = Scene::new();
    scene.set_background(Some(Vec3::splat(0.5)));
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default())
            .with_transform(Transform::looking_at(
                Vec3::new(0.0, 0.0, 6.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    let material = Material::default()
        .with_shader(Resource::new_ok("instance-colors", hook))
        .with_base_color(kmath::Vec4::ONE);
    let lit = kmath::Vec4::new(1.0, 0.0, 0.0, 0.0);
    // 节点本身往上挪 0.5：实例的世界位置 = 节点 × 实例，两层都得算上。
    scene.add_node(
        Node::new("Row")
            .with_mesh(Mesh::cube())
            .with_material(material)
            .with_position(Vec3::new(0.0, 0.5, 0.0))
            .with_instances(vec![
                Instance::at(Vec3::new(-2.0, -0.5, 0.0))
                    .with_color(Vec3::new(1.0, 0.0, 0.0))
                    .with_data(lit),
                Instance::at(Vec3::new(0.0, -0.5, 0.0))
                    .with_color(Vec3::new(0.0, 1.0, 0.0))
                    .with_data(lit),
                // 数据是 0：钩子输出黑色。
                Instance::at(Vec3::new(2.0, -0.5, 0.0)).with_color(Vec3::new(0.0, 0.0, 1.0)),
            ]),
    );
    scene.update();
    let ui = empty_ui();
    for _ in 0..2 {
        renderer.render(&scene, &ui, &[]);
    }
    let (_, _, pixels) = renderer.read_pixels().unwrap();
    let at = |x: usize| {
        let i = (32 * 64 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    };
    // 64 像素宽：左边的方块占 0..12 列，中间的 26..38，右边的 50..64（实测）。
    let (left, middle, right) = (at(4), at(32), at(58));
    assert!(
        left[0] > 150 && left[1] < 40 && left[2] < 40,
        "左边该是红的：{left:?}"
    );
    assert!(
        middle[1] > 150 && middle[0] < 40 && middle[2] < 40,
        "中间该是绿的：{middle:?}"
    );
    assert!(
        right.iter().all(|&c| c < 45),
        "右边的 instance_data 是 0，该是黑的：{right:?}"
    );
    // 三个方块之间的缝里是背景（灰）：实例没被画成一大块。
    let gap = at(20);
    assert!(gap.iter().all(|&c| c > 60), "两个方块之间该是背景：{gap:?}");
    // 一个节点、一次绘制、三份。
    let stats = renderer.stats();
    assert_eq!(stats.draw_calls, 1);
    assert_eq!(stats.drawn, 3);
    assert_eq!(stats.triangles, 36);
}

#[test]
fn an_instanced_cube_looks_and_shadows_exactly_like_a_plain_one() {
    use kscene::Instance;
    // 阴影 pass 走的是另一条路（CPU 上把「节点 × 实例」乘好，一个槽一个阴影对象），
    // 和主 pass 的槽对不上的话，影子会落在节点原点、或者整个没有。
    let plain = Node::new("Cube")
        .with_mesh(Mesh::cube())
        .with_position(Vec3::new(0.0, 1.0, 0.0));
    let instanced = || {
        Node::new("Cube")
            .with_mesh(Mesh::cube())
            .with_position(Vec3::new(0.0, 0.5, 0.0))
            .with_instances(vec![Instance::at(Vec3::new(0.0, 0.5, 0.0))])
    };
    let Some(a) = shadow_scene_pixels(plain, true) else {
        return;
    };
    let b = shadow_scene_pixels(instanced(), true).unwrap();
    let changed = changed_pixels(&a, &b);
    assert!(
        changed <= 4,
        "实例化的方块和普通方块画得不一样（{changed} 个像素）"
    );
    // 影子确实在：关掉投影差一大片。
    let no_shadow = shadow_scene_pixels(instanced(), false).unwrap();
    assert!(changed_pixels(&b, &no_shadow) > 100, "实例化的方块没投影");
}
