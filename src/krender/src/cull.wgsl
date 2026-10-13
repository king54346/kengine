// GPU 逐实例剔除：每个（槽, 视图）一个线程，看得见的槽压紧写进那个视图、那个批次的那一段，
// 顺手把间接绘制参数里的实例数加一。
//
// 视图 0 是相机，1.. 是阴影的各层（主光源的级联 / 立方体面，然后是额外投影光源的面）。
// 压紧结果 `culled` 一个视图占一段（`槽总数` 个槽），批次在段里的起点和它在原始槽数组里一样
// （`batch.first`），所以间接参数的 `first_instance = 视图 × 槽总数 + batch.first` 由 CPU 预填，
// 这里只累加 `instance_count`。各个 pass 绑的 `instance_slots` 就是 `culled`：着色器拿
// `instance_index` 查槽，和不剔除时一模一样。
//
// 判据和 CPU 上的 `klight::cascade::shadow_visibility` 是同一个：包围盒八个角投到裁剪空间，
// 取它们的包围盒和裁剪立方体求交，再按投影尺寸剔掉太小的（只有阴影层设了下限）。
// 相机视图里非实例化的槽 CPU 已经剔过了，直接算看得见。

struct CullParams {
    // x = 槽总数，y = 每个视图的批次数，z = 视图数
    counts: vec4<u32>,
};

struct CullBatch {
    first: u32,
    count: u32,
    // 第 0 位：相机视图里保持原顺序（半透明），原样拷过去、不剔除
    flags: u32,
    _pad: u32,
};

struct CullView {
    matrix: mat4x4<f32>,
    // x = 阴影图边长，y = 投影尺寸下限（纹素，0 = 不按尺寸剔）
    params: vec4<f32>,
};

struct ObjectBounds {
    model: mat4x4<f32>,
    // 网格的局部包围盒（实例化对象用）。local_min.w = 1：不准（蒙皮、形变），不按实例测
    local_min: vec4<f32>,
    local_max: vec4<f32>,
    // 世界包围盒（非实例化对象在阴影层里用）。world_min.w = 0：不投影
    world_min: vec4<f32>,
    world_max: vec4<f32>,
};

// 和 `kscene::Instance` 逐字节一致。
struct InstanceData {
    transform: mat4x4<f32>,
    color: vec4<f32>,
    data: vec4<f32>,
};

// 和 `wgpu::util::DrawIndexedIndirectArgs` 逐字节一致。
struct DrawArgs {
    index_count: u32,
    instance_count: atomic<u32>,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};

const NO_INSTANCE: u32 = 0xffffffffu;
const NO_BATCH: u32 = 0xffffffffu;

@group(0) @binding(0) var<uniform> params: CullParams;
@group(0) @binding(1) var<storage, read> source: array<vec2<u32>>;
@group(0) @binding(2) var<storage, read> slot_batch: array<u32>;
@group(0) @binding(3) var<storage, read> batches: array<CullBatch>;
@group(0) @binding(4) var<storage, read> views: array<CullView>;
@group(0) @binding(5) var<storage, read> bounds: array<ObjectBounds>;
@group(0) @binding(6) var<storage, read> instances: array<InstanceData>;
@group(0) @binding(7) var<storage, read_write> culled: array<vec2<u32>>;
@group(0) @binding(8) var<storage, read_write> args: array<DrawArgs>;

// 包围盒 `[lo, hi]` 经 `m` 变换后看不看得见。和 `shadow_visibility` 逐条对应。
fn box_visible(m: mat4x4<f32>, lo: vec3<f32>, hi: vec3<f32>, view: CullView) -> bool {
    var low = vec3<f32>(1e30);
    var high = vec3<f32>(-1e30);
    var behind = 0u;
    for (var i = 0u; i < 8u; i++) {
        let corner = vec3<f32>(
            select(lo.x, hi.x, (i & 1u) != 0u),
            select(lo.y, hi.y, (i & 2u) != 0u),
            select(lo.z, hi.z, (i & 4u) != 0u),
        );
        let clip = m * vec4<f32>(corner, 1.0);
        // 级联（正交）的 w 恒为 1；相机、点光、聚光是透视的，要除 w。
        if clip.w <= 1e-6 {
            behind += 1u;
            continue;
        }
        let ndc = clip.xyz / clip.w;
        low = min(low, ndc);
        high = max(high, ndc);
    }
    // 全在背后：看不见。一部分在背后：跨过了视点所在的平面，投影没意义，保守算看得见。
    if behind == 8u {
        return false;
    }
    if behind > 0u {
        return true;
    }
    // 裁剪立方体：XY 是 [-1, 1]，Z 是 [0, 1]。
    if high.x < -1.0 || low.x > 1.0 || high.y < -1.0 || low.y > 1.0 || high.z < 0.0 || low.z > 1.0 {
        return false;
    }
    if view.params.y > 0.0 {
        let extent = high.xy - low.xy;
        if max(extent.x, extent.y) * view.params.x * 0.5 < view.params.y {
            return false;
        }
    }
    return true;
}

fn slot_visible(slot: vec2<u32>, view_index: u32) -> bool {
    let object = bounds[slot.x];
    let view = views[view_index];
    let camera = view_index == 0u;
    if slot.y == NO_INSTANCE {
        // 相机：CPU 已经按节点剔过。阴影层：按对象的世界包围盒判（不投影的直接剔掉）。
        if camera {
            return true;
        }
        return object.world_min.w > 0.5
            && box_visible(view.matrix, object.world_min.xyz, object.world_max.xyz, view);
    }
    if !camera && object.world_min.w < 0.5 {
        return false;
    }
    // 局部包围盒不准（蒙皮、形变）：相机里算看得见，阴影层里按整组的世界包围盒判。
    if object.local_min.w > 0.5 {
        return camera || box_visible(view.matrix, object.world_min.xyz, object.world_max.xyz, view);
    }
    let m = view.matrix * object.model * instances[slot.y].transform;
    return box_visible(m, object.local_min.xyz, object.local_max.xyz, view);
}

@compute @workgroup_size(64)
fn cull(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot_count = params.counts.x;
    let batch_count = params.counts.y;
    let s = id.x;
    let view_index = id.y;
    if s >= slot_count || view_index >= params.counts.z {
        return;
    }
    let b = slot_batch[s];
    if b == NO_BATCH {
        return;
    }
    let batch = batches[b];
    let slot = source[s];
    // 相机视图里的半透明批次：原样拷到同一个位置，实例数 CPU 已经填好，顺序不变。
    if view_index == 0u && (batch.flags & 1u) != 0u {
        culled[s] = slot;
        return;
    }
    if !slot_visible(slot, view_index) {
        return;
    }
    let at = atomicAdd(&args[view_index * batch_count + b].instance_count, 1u);
    culled[view_index * slot_count + batch.first + at] = slot;
}
