//! 例子里那些自定义材质钩子，拼进引擎的标准着色器之后编不编得过。
//!
//! # 为什么需要这个
//!
//! 材质钩子是一段**片段**，单独拿去解析必然失败（它引用引擎定义的
//! `Surface`、`globals`、各张贴图）。所以 kengine 不在加载时校验它，
//! 而是等渲染器把它和标准着色器拼起来——**那意味着写错的钩子在跑起来
//! 之前一声不吭**，跑起来之后也只是日志里一行「退回标准管线」，
//! 画面看着像是效果没写对。
//!
//! 这份测试把那一刻提前到 `cargo test`。每个例子的 `.wgsl` 都在这里
//! 走一遍 [`krender::validate_material_hook`]，改坏了立刻红。
//!
//! # 覆盖的是拼装，不是画面
//!
//! 编译得过不代表画得对——颜色算错、法线方向反了、参数槽位对错了号，
//! 这些都编译得过。那些只能靠人看，见 `next.md` 最后一节。

use kengine::krender::validate_material_hook;
use kengine::kshader::Shader;

/// 拼一遍，失败就带着着色器名字报出来。
fn check(name: &str, hook: &str) {
    if let Err(error) = validate_material_hook(hook) {
        panic!("{name} 拼进标准着色器之后编不过：\n{error}");
    }
}

#[test]
fn every_example_hook_compiles() {
    for (name, source) in [
        (
            "shader/animate_shader.wgsl",
            include_str!("../examples/kengine/shader/animate_shader.wgsl"),
        ),
        (
            "shader/extended_material.wgsl",
            include_str!("../examples/kengine/shader/extended_material.wgsl"),
        ),
        (
            "shader/shader_material.wgsl",
            include_str!("../examples/kengine/shader/shader_material.wgsl"),
        ),
        (
            "shader/screenspace_texture.wgsl",
            include_str!("../examples/kengine/shader/screenspace_texture.wgsl"),
        ),
        (
            "shader/array_texture.wgsl",
            include_str!("../examples/kengine/shader/array_texture.wgsl"),
        ),
        (
            "shader/pipeline_constants.wgsl",
            include_str!("../examples/kengine/shader/pipeline_constants.wgsl"),
        ),
        (
            "shader/shader_prepass.wgsl",
            include_str!("../examples/kengine/shader/shader_prepass.wgsl"),
        ),
        (
            "shader/depth_probe.wgsl",
            include_str!("../examples/kengine/shader/depth_probe.wgsl"),
        ),
        ("water.wgsl", include_str!("../examples/kengine/water.wgsl")),
        // 这一份覆盖的是**另一个钩子**（`material_lighting`）。
        // 加进来是因为两个钩子的拼装是分开判断的：只写光照钩子时
        // 引擎要补上默认的表面钩子，反过来也一样。补错了整份材质编译不过，
        // 而例子只有跑起来才会暴露。
        (
            "new/lights_custom.wgsl",
            include_str!("../examples/kengine/new/lights_custom.wgsl"),
        ),
    ] {
        check(name, source);
    }
}

#[test]
fn hooks_can_forward_reference_the_engine_helpers() {
    // `scene_depth` / `scene_color` 声明在 `shader.wgsl` 里，而钩子是拼在
    // **它前面**的。WGSL 的模块级声明允许前向引用，所以这样是合法的——
    // 但这条依赖很容易在重排拼接顺序时被打破，而症状是所有读深度的
    // 材质一起编不过。钉一条测试在这里。
    check(
        "forward reference",
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            let behind = scene_depth(surface.screen_uv);
            let color = scene_color(surface.screen_uv);
            out.emissive = color * (behind - surface.view_depth);
            return out;
        }
        "#,
    );
}

#[test]
fn the_shader_defs_hook_compiles_in_every_combination() {
    // 四个开关一共 16 种组合。**每一种都要单独验**——`#ifdef` 删掉的代码
    // 不参与编译，所以「开着能编过」说明不了「关着也能编过」，
    // 反之亦然（关掉之后某个变量可能变成没人用，或者某个分支缺了返回值）。
    const DEFS: [&str; 4] = ["STRIPES", "FRESNEL", "PULSE", "METAL"];
    let source = include_str!("../examples/kengine/shader/shader_defs.wgsl");

    for mask in 0..(1u32 << DEFS.len()) {
        let active: Vec<&str> = DEFS
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, name)| *name)
            .collect();

        let shader = Shader::snippet_with_defs(source, &active)
            .unwrap_or_else(|error| panic!("组合 {active:?} 的条件编译指令有问题：{error}"));

        check(&format!("shader_defs.wgsl {active:?}"), shader.source());
    }
}

#[test]
fn the_pipeline_constants_hook_declares_the_constants_the_example_sets() {
    // Rust 侧 `with_constant("LEVELS", ..)` 和 WGSL 侧 `override LEVELS`
    // 是靠名字对上的。名字打错不会报错——驱动会**忽略**不认识的常量名，
    // 于是着色器安静地用着默认值，五块板子长得一模一样。
    let source = include_str!("../examples/kengine/shader/pipeline_constants.wgsl");

    assert!(
        source.contains("override LEVELS"),
        "例子设的是 LEVELS，着色器里却没有这个 override"
    );
}

#[test]
fn the_compute_shaders_pass_validation_on_their_own() {
    // 计算着色器和材质钩子不同：它是完整的一份源码，能自己解析。
    for (name, source) in [
        (
            "compute_game_of_life.wgsl",
            include_str!("../examples/kengine/shader/compute_game_of_life.wgsl"),
        ),
        (
            "gpu_readback.wgsl",
            include_str!("../examples/kengine/shader/gpu_readback.wgsl"),
        ),
    ] {
        if let Err(error) = Shader::from_wgsl(source) {
            panic!("{name} 校验失败：\n{error}");
        }
    }
}

#[test]
fn the_gpu_readback_bucket_count_matches_both_sides() {
    // Rust 侧按 8 个桶去解读回来的字节，WGSL 侧按 `BUCKETS` 去分桶。
    // 两边对不上的话，多出来的桶永远是 0，少了的那些会挤进最后一个——
    // 而两边各自都编译得过。
    let wgsl = include_str!("../examples/kengine/shader/gpu_readback.wgsl");
    let rust = include_str!("../examples/kengine/shader/gpu_readback.rs");

    assert!(wgsl.contains("const BUCKETS: u32 = 8u;"));
    assert!(rust.contains("const BUCKETS: usize = 8;"));
}

#[test]
fn the_gpu_readback_shader_counts_every_pixel_exactly_once() {
    // 这个例子的 UI 上有一行「计数对得上」，靠的是原子加。
    // 漏掉 `atomic` 的话总数会**偏小而且每次跑都不一样**——
    // 那种错在画面上完全看不出来，只能靠这条断言。
    use kengine::krender::{ComputeBinding, ComputeContext, StorageFormat};

    // CI 上通常没有 GPU，那种环境该跳过而不是红。
    // 共用整个测试进程的那一台设备，见 `shared_headless` 的文档。
    let Some(gpu) = ComputeContext::shared_headless() else {
        return;
    };

    let shader = Shader::from_wgsl(include_str!("../examples/kengine/shader/gpu_readback.wgsl"))
        .expect("计算着色器应当通过校验");
    let pipeline = gpu.create_pipeline(&shader).expect("管线该建得出来");

    // 故意用一个**不是 8 的倍数**的尺寸：工作组是 8×8，
    // 边界检查写错的话最后一行一列不会被统计。
    const SIZE: u32 = 60;
    const BUCKETS: usize = 8;

    let histogram = gpu.create_buffer_zeroed("histogram", (BUCKETS * 4) as u64);
    let frame = gpu.create_buffer("frame", &7u32.to_le_bytes());
    let image = gpu.create_storage_texture("image", SIZE, SIZE, StorageFormat::Rgba8Unorm);

    gpu.dispatch_with(
        &pipeline,
        &[
            ComputeBinding::Buffer(&histogram),
            ComputeBinding::Texture(&image),
            ComputeBinding::Buffer(&frame),
        ],
        [SIZE.div_ceil(8), SIZE.div_ceil(8), 1],
    );

    let bytes = gpu.read(&histogram).expect("该读得回来");
    let total: u32 = bytes
        .chunks_exact(4)
        .take(BUCKETS)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .sum();

    assert_eq!(total, SIZE * SIZE, "有像素没被数到，或者被数了两次");

    // 纹理那条路也得对得上：读回来的必须是紧密排列的像素，
    // 60 × 4 = 240 字节一行，而 GPU 那边每行占了 256 字节。
    let pixels = gpu.read_texture(&image).expect("该读得回来");
    assert_eq!(pixels.len(), (SIZE * SIZE * 4) as usize);
    assert!(
        pixels.chunks_exact(4).all(|p| p[3] == 255),
        "有像素没被写到 —— 边界检查把最后一行一列漏掉了"
    );
}
