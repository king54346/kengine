//! 场景存读盘。
//!
//! ```bash
//! cargo run --example scene_roundtrip                # 交互：F5 存，F9 读
//! cargo run --example scene_roundtrip -- --headless  # 无窗口自检，跑完退出
//! ```
//!
//! # 存的是什么
//!
//! 节点树、变换、网格、材质、光源、相机、物理组件、粒子设置。
//! 网格与材质**去重共享**：一百个共用同一份网格的方块，文件里只有一份几何。
//!
//! # 不存的是什么
//!
//! - **派生数据**：世界变换、包围盒、剔除结构、组件索引。读回来之后
//!   [`Scene::update`] 会重算它们，存进去只是浪费空间和引入不一致的机会。
//! - **运行时状态**：脚本实例的闭包变量、正在播的音频进度。
//! - **调试线**：即时模式的，每帧都重画。
//!
//! # `--headless` 那条路径存在的理由
//!
//! 「读回来的场景和存盘前一模一样」这句话**没法靠肉眼确认**——
//! 画面看着一样不代表数据一样。所以这里做的是**数值对拍**：
//! 比节点数、比每个节点的世界变换、比包围盒。

use kengine::prelude::*;

fn scene_path() -> std::path::PathBuf {
    std::env::temp_dir().join("kengine_roundtrip.bin")
}

/// 搭一个有代表性的场景：共享网格、几种材质、光、相机、物理。
fn populate(scene: &mut Scene) {
    scene.add_node(
        Node::new("Camera")
            .with_camera(Camera::default())
            .with_transform(Transform::looking_at(
                Vec3::new(0.0, 4.0, 10.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    scene.add_node(
        Node::new("Sun")
            .with_light(Light::directional().with_intensity(2.5))
            .with_transform(Transform::looking_at(
                Vec3::new(3.0, 6.0, 4.0),
                Vec3::ZERO,
                Vec3::Y,
            )),
    );
    scene.add_node(
        Node::new("Ground")
            .with_mesh(Mesh::cube())
            .with_material(PbrMaterial::metal(Vec3::splat(0.3), 0.9))
            .with_scale(Vec3::new(20.0, 0.2, 20.0))
            .with_collider(Collider::cuboid(Vec3::new(10.0, 0.1, 10.0)))
            .with_position(Vec3::new(0.0, -0.1, 0.0)),
    );

    // 一片共用同一份网格的方块。网格克隆共享同一个 id，
    // 存盘时只写一份几何——这正是「去重共享」要验证的东西。
    let mesh = Mesh::cube();
    let mut parent = scene.root();
    for i in 0..40 {
        let node = scene.add_node_with_parent(
            Node::new(format!("Crate{i}"))
                .with_mesh(mesh.clone())
                .with_material(PbrMaterial::metal(
                    Vec3::new(0.8, 0.5, 0.2),
                    (i % 8) as f32 / 8.0,
                ))
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::cuboid(Vec3::splat(0.5)))
                .with_position(Vec3::new(
                    (i % 8) as f32 * 1.3 - 4.5,
                    1.0 + (i / 8) as f32 * 1.1,
                    0.0,
                )),
            parent,
        );
        // 每隔八个挂到上一个下面，制造一点层级——平铺的树验证不了
        // 父子关系是否被正确保留。
        if i % 8 == 7 {
            parent = node;
        }
    }
}

/// 无窗口自检：存 → 读 → 逐节点对拍。
fn headless() {
    let mut original = Scene::new();
    populate(&mut original);
    original.update();
    // 跑几步物理，让位置不再是初始值——初始值全都对得上是废话，
    // 得让它们变成「算出来的」才有对拍的意义。
    for _ in 0..60 {
        original.step_physics(1.0 / 60.0);
        original.update();
    }

    let path = scene_path();
    original.save(&path).expect("存盘应当成功");
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let mut loaded = Scene::load(&path, None).expect("读档应当成功");
    loaded.update();

    // ── 对拍 ──
    let mut mismatches = 0;
    assert_eq!(
        original.nodes().alive_count(),
        loaded.nodes().alive_count(),
        "节点数对不上"
    );

    for handle in original.drawable_nodes() {
        let a = original.try_get(*handle).expect("原场景里应当有这个节点");
        let Some(b) = loaded.find_by_name(&a.name).and_then(|h| loaded.try_get(h)) else {
            klog::error!("读回来的场景里找不到节点 {}", &a.name);
            mismatches += 1;
            continue;
        };

        // 比世界变换而不是局部变换：世界变换是派生出来的，
        // 它对得上，说明层级和局部变换都对得上。
        let da = a.global_transform().w_axis.truncate();
        let db = b.global_transform().w_axis.truncate();
        if (da - db).length() > 1e-4 {
            klog::error!("{} 的位置对不上：{da:?} vs {db:?}", &a.name);
            mismatches += 1;
        }
        if (a.global_aabb().center() - b.global_aabb().center()).length() > 1e-4 {
            klog::error!("{} 的包围盒对不上", &a.name);
            mismatches += 1;
        }
    }

    let nodes = original.nodes().alive_count();
    klog::info!(
        "自检：{nodes} 个节点，{:.1} KB（平均 {:.0} 字节/节点）",
        size as f64 / 1024.0,
        size as f64 / nodes as f64,
    );
    if mismatches == 0 {
        klog::info!("存读盘一致 ✓");
    } else {
        klog::error!("有 {mismatches} 处对不上 ✗");
        std::process::exit(1);
    }
}

#[derive(Default)]
struct RoundtripDemo;

impl Plugin for RoundtripDemo {
    fn init(&mut self, ctx: &mut Context) {
        ctx.input.bindings_mut().bind_action("save", KeyCode::F5);
        ctx.input.bindings_mut().bind_action("load", KeyCode::F9);
        populate(ctx.scene);
        klog::info!("F5 存盘，F9 读档，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        if ctx.input.action_just_pressed("save") {
            match ctx.scene.save(scene_path()) {
                Ok(()) => klog::info!("已存到 {}", scene_path().display()),
                Err(e) => klog::error!("存盘失败：{e:?}"),
            }
        }

        if ctx.input.action_just_pressed("load") {
            // 传资源管理器进去：场景里引用了外部资源（贴图、外部网格）时，
            // 靠它把路径解析回资源句柄。全内联的场景给 None 也行。
            match Scene::load(scene_path(), Some(ctx.resources)) {
                Ok(scene) => {
                    // 整个换掉。读回来的场景里派生数据是空的，
                    // 引擎下一帧的 `update` 会把它们算出来。
                    *ctx.scene = scene;
                    klog::info!("已读档");
                }
                Err(e) => klog::error!("读档失败：{e:?}"),
            }
        }

        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

fn main() {
    klog::init(None);

    if std::env::args().any(|a| a == "--headless") {
        headless();
        return;
    }

    App::new()
        .with_title("kengine — scene roundtrip")
        .add_plugin(RoundtripDemo)
        .run();
}
