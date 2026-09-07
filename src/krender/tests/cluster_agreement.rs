//! 聚簇的两半必须对得上：CPU 决定「哪盏灯进哪个簇」，着色器决定
//! 「这个片元属于哪个簇」。
//!
//! # 为什么这条要真跑 GPU
//!
//! 两边算错时会发生什么：
//!
//! - **不越界**（下标仍在合法范围里）
//! - **不报错**（wgpu 什么都不会说）
//! - **不掉帧**（工作量一模一样）
//!
//! 只是画面上少了几盏灯的贡献。这正是这个仓库反复记的那类错误——
//! 装上了，看着在跑，其实错的。
//!
//! # 验的是「覆盖」，不是「相等」
//!
//! 一开始这里验的是「两边算出同一个下标」，第一次跑就红了：
//! 片元落在块边界上时，某块 GPU 把 `960 * 16 / 1920` 算成了
//! **7.9999995**（驱动重排成了乘以倒数），取整得 7，而 CPU 得 8。
//!
//! 那不是「写得不一样」——浮点重排不受源码控制，换块显卡结论可能就变。
//! 所以要求改成了真正要紧的那条：
//!
//! > 片元被某盏灯照到时，**着色器挑中的那个簇里必须有这盏灯**。
//!
//! CPU 那边为此把每盏灯的块范围往外扩了一格（见
//! `klight::cluster::screen_tiles` 的文档），边界上两边选哪边都取得到。
//!
//! 没有可用显卡时整条跳过——CI 上通常没有，本地一定跑得到。

use klight::cluster::{ClusterGrid, ClusterLight};
use kmath::{Mat4, Vec3};

const WIDTH: f32 = 1920.0;
const HEIGHT: f32 = 1080.0;

/// 把 `cluster.wgsl` 包成一个能跑的计算着色器。
///
/// 只拼那一个文件：塞进主着色器里的话，测它就得把整套光照、PBR、
/// 阴影一起拖进来，而那些和这条要验的东西无关。
fn shader_source() -> String {
    [
        klight::CLUSTER_WGSL,
        r#"
struct Input {
    // xy = 像素坐标，z = 视空间深度，w 未用
    pixel_depth: vec4<f32>,
};

struct Params {
    // xy = 视口尺寸，zw 未用
    viewport: vec4<f32>,
    // xy = 分块数，z = 切片数，w 未用
    grid: vec4<u32>,
    // x = 近平面，y = 1 / ln(far / near)，zw 未用
    depth: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> inputs: array<Input>;
@group(0) @binding(1) var<storage, read_write> outputs: array<u32>;
@group(0) @binding(2) var<storage, read> params: array<Params>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= arrayLength(&outputs)) { return; }
    let p = params[0];
    let item = inputs[id.x];
    outputs[id.x] = cluster_index(
        item.pixel_depth.xy,
        p.viewport.xy,
        item.pixel_depth.z,
        p.grid.xy,
        p.grid.z,
        p.depth.x,
        p.depth.y,
    );
}
"#,
    ]
    .join("\n")
}

/// 跑一遍着色器，返回每个输入算出来的簇下标。
///
/// 没有可用显卡时返回 [`None`]。
fn shader_clusters(grid: &ClusterGrid, inputs: &[[f32; 4]]) -> Option<Vec<u32>> {
    // 共用整个测试进程的那一台设备，见 `shared_headless` 的文档。
    let gpu = krender::ComputeContext::shared_headless()?;
    let shader = kshader::Shader::from_wgsl(shader_source()).expect("聚簇着色器应当通过校验");
    let pipeline = gpu.create_pipeline(&shader).expect("管线该建得出来");

    let input_buffer = gpu.create_buffer("inputs", bytemuck::cast_slice(inputs));
    let output_buffer = gpu.create_buffer_zeroed("outputs", (inputs.len() * 4) as u64);

    // 和 WGSL 里的 `Params` 逐字段对应。字段顺序或填充对不上不会报错，
    // 只是读出来全是垃圾。
    let params: [u32; 12] = [
        WIDTH.to_bits(),
        HEIGHT.to_bits(),
        0,
        0,
        grid.tiles_x,
        grid.tiles_y,
        grid.slices,
        0,
        grid.near.to_bits(),
        (1.0 / (grid.far / grid.near).ln()).to_bits(),
        0,
        0,
    ];
    let params_buffer = gpu.create_buffer("params", bytemuck::cast_slice(&params));

    gpu.dispatch(
        &pipeline,
        &[&input_buffer, &output_buffer, &params_buffer],
        [(inputs.len() as u32).div_ceil(64), 1, 1],
    );

    let bytes = gpu.read(&output_buffer)?;
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

fn grid() -> ClusterGrid {
    ClusterGrid {
        tiles_x: 16,
        tiles_y: 9,
        slices: 24,
        near: 0.1,
        far: 200.0,
    }
}

fn view() -> Mat4 {
    Mat4::look_at_rh(Vec3::new(0.0, 0.0, 0.0), Vec3::NEG_Z, Vec3::Y)
}

fn projection() -> Mat4 {
    Mat4::perspective_rh(60_f32.to_radians(), WIDTH / HEIGHT, 0.1, 200.0)
}

/// 世界坐标 → （像素坐标, 视空间深度）。着色器拿到的就是这两样。
fn to_fragment(world: Vec3) -> Option<[f32; 4]> {
    let view_position = view().transform_point3(world);
    let depth = -view_position.z;
    if depth <= 0.1 {
        return None;
    }
    let clip = projection() * view_position.extend(1.0);
    if clip.w <= 1e-6 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !(-1.0..=1.0).contains(&ndc.x) || !(-1.0..=1.0).contains(&ndc.y) {
        return None;
    }
    Some([
        (ndc.x * 0.5 + 0.5) * WIDTH,
        (0.5 - ndc.y * 0.5) * HEIGHT,
        depth,
        0.0,
    ])
}

#[test]
fn every_lit_fragment_finds_its_light_in_its_own_cluster() {
    // 这是整条路的验收：片元被某盏灯照到时，着色器挑中的那个簇里
    // **必须有这盏灯**。少了就是画面上少一块光，而且不报任何错。
    let grid = grid();

    // 一批位置、半径都不一样的灯，覆盖近处、远处、屏幕中央和边缘。
    let lights: Vec<ClusterLight> = (0..24)
        .map(|i| {
            let t = i as f32;
            ClusterLight {
                position: Vec3::new((t * 0.9).sin() * 6.0, (t * 1.3).cos() * 3.0, -1.5 - t * 1.7),
                radius: 0.8 + (t * 0.37).fract() * 2.5,
            }
        })
        .collect();

    let assignment = klight::cluster::assign(&grid, &lights, view(), projection());

    // 每盏灯取一批它**确实照得到**的点：球心，以及球内几个方向上的点。
    // 半径取 0.7 而不是 1.0：球面上的点在边界上，那里「照不照得到」
    // 本来就是模糊的，验它没有意义。
    let mut samples: Vec<(usize, [f32; 4])> = Vec::new();
    for (index, light) in lights.iter().enumerate() {
        let offsets = [
            Vec3::ZERO,
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
            Vec3::new(0.577, 0.577, 0.577),
        ];
        for offset in offsets {
            let point = light.position + offset * light.radius * 0.7;
            if let Some(fragment) = to_fragment(point) {
                samples.push((index, fragment));
            }
        }
    }
    assert!(samples.len() > 50, "样本太少，这条测试就没意义了");

    let inputs: Vec<[f32; 4]> = samples.iter().map(|(_, f)| *f).collect();
    let Some(clusters) = shader_clusters(&grid, &inputs) else {
        return; // 没有可用显卡，跳过
    };

    let mut missing = Vec::new();
    for ((light_index, fragment), &cluster) in samples.iter().zip(&clusters) {
        let list = assignment.cluster(cluster as usize);
        if !list.contains(&(*light_index as u32)) {
            missing.push(format!(
                "  第 {light_index} 盏灯照到了像素 ({:.1}, {:.1}) 深度 {:.2}，\
                 但着色器挑的簇 {cluster} 的名单里没有它",
                fragment[0], fragment[1], fragment[2]
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "有 {} 处「照得到但不在名单里」（画面上会少一块光）：\n{}",
        missing.len(),
        missing
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_shader_and_the_cpu_never_differ_by_more_than_one_tile() {
    // 边界上差一格是浮点重排的必然结果，扩一格已经兜住了。
    // 但差**两格以上**说明两边的公式真的不一样了——那种错扩多少格都兜不住。
    let grid = grid();

    let mut inputs: Vec<[f32; 4]> = Vec::new();
    for &pixel in &[
        [0.5, 0.5],
        [1919.5, 1079.5],
        [960.5, 540.5],
        [960.0, 540.0],
        [123.5, 456.5],
    ] {
        for slice in 0..grid.slices {
            let (start, end) = grid.slice_range(slice);
            inputs.push([pixel[0], pixel[1], (start + end) * 0.5, 0.0]);
            inputs.push([pixel[0], pixel[1], start * 1.0001, 0.0]);
        }
        inputs.push([pixel[0], pixel[1], 0.001, 0.0]);
        inputs.push([pixel[0], pixel[1], 1.0e6, 0.0]);
    }

    let Some(clusters) = shader_clusters(&grid, &inputs) else {
        return;
    };

    // CPU 侧按同样的口径算一遍。
    let cpu = |pixel: [f32; 2], depth: f32| -> [u32; 3] {
        let tile = |value: f32, size: f32, count: u32| -> u32 {
            ((value * count as f32 / size).floor() as i64).clamp(0, count as i64 - 1) as u32
        };
        [
            tile(pixel[0], WIDTH, grid.tiles_x),
            tile(pixel[1], HEIGHT, grid.tiles_y),
            grid.slice_of(depth),
        ]
    };

    for (item, &cluster) in inputs.iter().zip(&clusters) {
        let [x, y, slice] = cpu([item[0], item[1]], item[2]);
        // 从着色器给的一维下标反解回三维。
        let gpu_x = cluster % grid.tiles_x;
        let gpu_y = (cluster / grid.tiles_x) % grid.tiles_y;
        let gpu_slice = cluster / (grid.tiles_x * grid.tiles_y);

        assert_eq!(
            gpu_slice, slice,
            "深度切片对不上（像素 {:.1},{:.1} 深度 {:.4}）——\
             这一项两边用的是同一个公式，不该有差别",
            item[0], item[1], item[2]
        );
        assert!(
            gpu_x.abs_diff(x) <= 1 && gpu_y.abs_diff(y) <= 1,
            "块号差了不止一格：CPU ({x},{y})，着色器 ({gpu_x},{gpu_y})，\
             像素 ({:.1}, {:.1})",
            item[0],
            item[1]
        );
    }
}

#[test]
fn a_degenerate_grid_agrees_too() {
    // 网格参数可能从别处算出来。两边对 0 的处理必须一致——
    // 一边返回 0、另一边除零出 NaN 的话，那一整块画面会失去光照。
    let grid = ClusterGrid {
        tiles_x: 0,
        tiles_y: 0,
        slices: 0,
        near: 0.0,
        far: 0.0,
    };
    let inputs = vec![[100.0, 50.0, 5.0, 0.0], [0.0, 0.0, 0.0, 0.0]];

    let Some(clusters) = shader_clusters(&grid, &inputs) else {
        return;
    };
    assert!(
        clusters.iter().all(|&index| index == 0),
        "退化的网格该一律返回 0，实际 {clusters:?}"
    );
}
