//! 聚光灯 cookie 的投影，和矩形面光源的形状因子——两样都在 GPU 上
//! 跑**真的那份 WGSL**，拿 CPU 上独立算出来的答案对。
//!
//! # 为什么非得这么验
//!
//! 这两处错了都不报错：
//!
//! - cookie 的 UV 算错，图案只是**歪了或糊了**，画面照样出得来。
//!   而「歪了多少」在动态场景里根本分辨不出来——聚光灯本来就在动。
//! - 形状因子的**符号**反了，`max(.., 0.0)` 会把它压成 0，
//!   面光源就成了一块不发光的板子。这比算错还隐蔽：看起来像
//!   「忘了打开这盏灯」，而不是像一个 bug。
//!
//! 而且这两个函数都**只在 WGSL 里存在**，Rust 侧没有对应实现，
//! 单元测试够不着。所以这里造一个无头设备，把 `light.wgsl` 原样
//! 拼成计算着色器跑一遍。
//!
//! 光源数据是用 `Light::to_gpu` 打包、按 `GpuLight` 的内存布局直接
//! 塞进存储缓冲的——顺带把「Rust 的结构体和 WGSL 的 `Light` 逐字段
//! 对得上」也一起验了。字段错位不会报错，只会让所有结果变成垃圾。
//!
//! 没有可用显卡时整条跳过。

use kmath::{Mat4, Vec3};

/// 把 `light.wgsl` 包成一个能跑的计算着色器。
fn shader_source() -> String {
    [
        klight::LIGHT_WGSL,
        r#"
struct Query {
    // xyz = 着色点，w = 用第几盏灯
    point: vec4<f32>,
    // xyz = 表面法线，w 未用
    normal: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> probe_lights: array<Light>;
@group(0) @binding(1) var<storage, read> queries: array<Query>;
// xy = cookie 的 uv，z = 矩形形状因子，w 未用
@group(0) @binding(2) var<storage, read_write> results: array<vec4<f32>>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= arrayLength(&results)) { return; }
    let q = queries[id.x];
    let light = probe_lights[u32(q.point.w)];
    let uv = light_cookie_uv(light, q.point.xyz);
    let form = light_rect_form_factor(light, q.point.xyz, normalize(q.normal.xyz));
    results[id.x] = vec4<f32>(uv, form, 0.0);
}
"#,
    ]
    .join("\n")
}

/// 跑一遍着色器。没有可用显卡时返回 [`None`]。
fn evaluate(lights: &[klight::GpuLight], queries: &[[f32; 8]]) -> Option<Vec<[f32; 4]>> {
    // 共用整个测试进程的那一台设备。每个测试各开一台的话，
    // 并行退出时驱动会间歇性地把进程带走——见 `shared_headless` 的文档。
    let gpu = krender::ComputeContext::shared_headless()?;
    let shader = kshader::Shader::from_wgsl(shader_source()).expect("光照着色器应当通过校验");
    let pipeline = gpu.create_pipeline(&shader).expect("管线该建得出来");

    let light_buffer = gpu.create_buffer("lights", bytemuck::cast_slice(lights));
    let query_buffer = gpu.create_buffer("queries", bytemuck::cast_slice(queries));
    let result_buffer = gpu.create_buffer_zeroed("results", (queries.len() * 16) as u64);

    gpu.dispatch(
        &pipeline,
        &[&light_buffer, &query_buffer, &result_buffer],
        [(queries.len() as u32).div_ceil(64), 1, 1],
    );

    let bytes = gpu.read(&result_buffer)?;
    Some(
        bytes
            .chunks_exact(16)
            .map(|chunk| {
                let mut out = [0.0f32; 4];
                for (slot, raw) in out.iter_mut().zip(chunk.chunks_exact(4)) {
                    *slot = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                }
                out
            })
            .collect(),
    )
}

fn query(point: Vec3, light: usize, normal: Vec3) -> [f32; 8] {
    [
        point.x,
        point.y,
        point.z,
        light as f32,
        normal.x,
        normal.y,
        normal.z,
        0.0,
    ]
}

// ── cookie 的投影 ──

#[test]
fn the_cookie_centre_lands_on_the_light_axis() {
    // 光轴上的点必须取到贴图正中央。偏了的话整张图案就是偏的，
    // 而聚光灯在动的时候这件事看不出来。
    let light = klight::Light::spot(20.0, 12.0, 30.0).with_cookie(Some(0));
    // 灯在原点，朝 -Z（`looking_at` 的约定）。
    let transform = Mat4::look_to_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y).inverse();
    let gpu = light.to_gpu(transform);

    let queries = [
        query(Vec3::new(0.0, 0.0, -1.0), 0, Vec3::Y),
        query(Vec3::new(0.0, 0.0, -8.0), 0, Vec3::Y),
    ];
    let Some(results) = evaluate(&[gpu], &queries) else {
        return;
    };

    for (result, distance) in results.iter().zip([1.0, 8.0]) {
        assert!(
            (result[0] - 0.5).abs() < 1e-4 && (result[1] - 0.5).abs() < 1e-4,
            "光轴上 {distance} 米处的 uv 该是 (0.5, 0.5)，实际 ({}, {})",
            result[0],
            result[1]
        );
    }
}

#[test]
fn the_cookie_fills_the_cone_and_does_not_scale_with_distance() {
    // 图案该**贴在锥体上**：远近不同但同在锥面上的两个点，uv 必须一样。
    // 少了那个「除以到轴距离」的归一化就会退化成「离灯越远图案越小」，
    // 而近处看画面是对的——这种错要走到远处才发现。
    // 锥角是**角度制**（见 `Light::spot`），算正切时要先换成弧度。
    let outer_degrees = 30.0_f32;
    let outer = outer_degrees.to_radians();
    let light = klight::Light::spot(20.0, 12.0, outer_degrees).with_cookie(Some(0));
    let transform = Mat4::look_to_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y).inverse();
    let gpu = light.to_gpu(transform);

    // 锥面上的点：沿 +X 偏移 axial * tan(outer)。
    let edge = |axial: f32| Vec3::new(axial * outer.tan(), 0.0, -axial);
    let queries = [
        query(edge(1.0), 0, Vec3::Y),
        query(edge(9.0), 0, Vec3::Y),
        // 上方向：沿 +Y。
        query(Vec3::new(0.0, 3.0 * outer.tan(), -3.0), 0, Vec3::Y),
    ];
    let Some(results) = evaluate(&[gpu], &queries) else {
        return;
    };

    assert!(
        (results[0][0] - 1.0).abs() < 1e-3,
        "外锥边缘该落在 u = 1，实际 {}",
        results[0][0]
    );
    assert!(
        (results[0][0] - results[1][0]).abs() < 1e-3
            && (results[0][1] - results[1][1]).abs() < 1e-3,
        "同在锥面上、远近不同的两点 uv 该一致，实际 {:?} 和 {:?}",
        &results[0][..2],
        &results[1][..2]
    );
    // v 向下：世界坐标的「上」对应贴图的 v = 0。
    assert!(
        results[2][1] < 0.01,
        "灯的正上方该落在 v ≈ 0（贴图的 v 是向下的），实际 {}",
        results[2][1]
    );
}

#[test]
fn the_cookie_axes_follow_the_light_rotation() {
    // 把灯绕光轴转 90°，图案必须跟着转。`right` 是 CPU 侧从节点变换
    // 里取的，取错了图案的朝向就和灯脱钩——灯转了图案不转，
    // 这在静止画面里完全看不出来。
    let light = klight::Light::spot(20.0, 12.0, 30.0).with_cookie(Some(0));

    let upright = Mat4::look_to_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y).inverse();
    // 同一个朝向，但「上」换成了 +X：相当于绕光轴转了 90°。
    let rolled = Mat4::look_to_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::X).inverse();

    let lights = [light.to_gpu(upright), light.to_gpu(rolled)];
    // 同一个世界点，问两盏姿态不同的灯。
    let point = Vec3::new(0.4, 0.0, -2.0);
    let queries = [query(point, 0, Vec3::Y), query(point, 1, Vec3::Y)];

    let Some(results) = evaluate(&lights, &queries) else {
        return;
    };
    let (a, b) = (results[0], results[1]);
    assert!(
        (a[0] - b[0]).abs() > 0.1 || (a[1] - b[1]).abs() > 0.1,
        "灯绕光轴转了 90°，同一个点的 uv 却几乎没变（{a:?} vs {b:?}）——\
         说明 uv 的两根轴没跟着灯的旋转走"
    );
}

// ── 矩形面光源的形状因子 ──

/// 数值积分算出来的形状因子，当参照。
///
/// 把矩形切成小格，每格按 `cos θ_surface * cos θ_light / (π d²) * dA`
/// 累加。这是形状因子的定义式，和着色器里那个闭式解是**两条独立的路**
/// ——闭式解写错时数值积分不会跟着错。
///
/// 只在矩形整个位于着色点地平线之上时才有可比性：闭式解不做地平线裁剪，
/// 而数值积分会把每一小格的负余弦当成 0。这一处差异是真实存在的
/// （掠射角下面光源会偏亮），不是测试的问题，所以样本都挑在地平线之上。
fn reference_form_factor(
    center: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    half: (f32, f32),
    point: Vec3,
    normal: Vec3,
) -> f32 {
    const STEPS: usize = 240;
    let du = 2.0 * half.0 / STEPS as f32;
    let dv = 2.0 * half.1 / STEPS as f32;
    let area = du * dv;

    let mut total = 0.0;
    for i in 0..STEPS {
        for j in 0..STEPS {
            let u = -half.0 + (i as f32 + 0.5) * du;
            let v = -half.1 + (j as f32 + 0.5) * dv;
            let sample = center + right * u + up * v;

            let delta = point - sample;
            let distance_sq = delta.length_squared();
            let direction = delta / distance_sq.sqrt();

            // 面板只往 `forward` 一侧发光。
            let cos_light = direction.dot(forward);
            let cos_surface = (-direction).dot(normal);
            if cos_light <= 0.0 || cos_surface <= 0.0 {
                continue;
            }
            total += cos_light * cos_surface / (std::f32::consts::PI * distance_sq) * area;
        }
    }
    total
}

#[test]
fn the_rect_form_factor_matches_numerical_integration() {
    // 闭式解 vs 数值积分。差得多就说明那个多边形公式写错了——
    // 而写错了画面上只是「面光源偏亮或偏暗」，没人能靠眼睛断言。
    let width = 2.0;
    let height = 1.2;
    let light = klight::Light::rect(width, height, 30.0);

    // 面板在 y = 3 处朝下（-Y），照亮下方的地面。
    let transform = Mat4::look_to_rh(Vec3::new(0.0, 3.0, 0.0), Vec3::NEG_Y, Vec3::NEG_Z).inverse();
    let gpu = light.to_gpu(transform);

    let center = Vec3::new(0.0, 3.0, 0.0);
    let forward = Vec3::NEG_Y;
    let right = Vec3::from_array([gpu.right[0], gpu.right[1], gpu.right[2]]);
    let up = forward.cross(right);
    let half = (width * 0.5, height * 0.5);

    // 都在面板正下方一带，法线朝上——整块面板都在地平线之上。
    let points = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(0.6, 0.0, 0.3),
        Vec3::new(-1.2, 0.5, 0.0),
        Vec3::new(0.0, 1.0, 0.8),
        Vec3::new(2.0, 0.0, 0.0),
    ];
    let queries: Vec<[f32; 8]> = points.iter().map(|&p| query(p, 0, Vec3::Y)).collect();

    let Some(results) = evaluate(&[gpu], &queries) else {
        return;
    };

    for (point, result) in points.iter().zip(&results) {
        let expected = reference_form_factor(center, right, up, forward, half, *point, Vec3::Y);
        let shader = result[2];
        assert!(
            expected > 1e-3,
            "参照值本身就接近 0（{expected}），这个样本验不出东西"
        );
        // 2% 的容差：数值积分本身有离散误差，而闭式解是精确的。
        assert!(
            (shader - expected).abs() < expected * 0.02,
            "着色点 {point:?} 的形状因子对不上：着色器 {shader}，数值积分 {expected}"
        );
    }
}

#[test]
fn a_surface_behind_the_panel_gets_nothing() {
    // 面板是单面发光的。背面漏光会让「墙上挂一块面光源」照亮墙背后的房间。
    let light = klight::Light::rect(2.0, 2.0, 30.0);
    let transform = Mat4::look_to_rh(Vec3::new(0.0, 3.0, 0.0), Vec3::NEG_Y, Vec3::NEG_Z).inverse();
    let gpu = light.to_gpu(transform);

    let queries = [
        // 面板上方——在它背后。
        query(Vec3::new(0.0, 5.0, 0.0), 0, Vec3::NEG_Y),
        // 面板下方但法线背对着它。
        query(Vec3::new(0.0, 0.0, 0.0), 0, Vec3::NEG_Y),
    ];
    let Some(results) = evaluate(&[gpu], &queries) else {
        return;
    };

    assert_eq!(results[0][2], 0.0, "面板背后的点不该收到任何光");
    assert_eq!(results[1][2], 0.0, "法线背对面板的表面不该收到任何光");
}

#[test]
fn the_form_factor_stays_within_one() {
    // 形状因子是「面板占了多大一块半球」，物理上不可能超过 1。
    // 超过 1 意味着这盏灯在凭空造能量——贴着面板的那一层会过曝成纯白。
    let light = klight::Light::rect(6.0, 6.0, 30.0);
    let transform = Mat4::look_to_rh(Vec3::new(0.0, 3.0, 0.0), Vec3::NEG_Y, Vec3::NEG_Z).inverse();
    let gpu = light.to_gpu(transform);

    // 从贴着面板一直退到很远。
    let queries: Vec<[f32; 8]> = (0..24)
        .map(|i| query(Vec3::new(0.0, 3.0 - 0.01 - i as f32 * 0.5, 0.0), 0, Vec3::Y))
        .collect();

    let Some(results) = evaluate(&[gpu], &queries) else {
        return;
    };
    for (index, result) in results.iter().enumerate() {
        assert!(
            result[2] <= 1.0 + 1e-3 && result[2] >= 0.0,
            "第 {index} 个样本的形状因子是 {}，越界了",
            result[2]
        );
    }
    // 贴得最近时该接近 1（面板几乎铺满整个半球）。
    assert!(
        results[0][2] > 0.9,
        "贴着一块 6×6 的面板，形状因子该接近 1，实际 {}",
        results[0][2]
    );
    // 退远之后单调下降。
    for pair in results.windows(2) {
        assert!(
            pair[1][2] <= pair[0][2] + 1e-4,
            "退远之后形状因子反而变大了：{} → {}",
            pair[0][2],
            pair[1][2]
        );
    }
}
