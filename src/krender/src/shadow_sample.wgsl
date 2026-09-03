// 阴影贴图采样。
//
// 这里只有纯函数，所有资源都通过参数传入——因此可以安全地拼接到主着色器里，
// 不会与主着色器自己的绑定声明冲突。

// 把世界坐标变换到阴影贴图的纹理坐标。
// 返回 xy = UV，z = 该点在光空间中的深度。
fn shadow_project(light_view_proj: mat4x4<f32>, world_position: vec3<f32>) -> vec3<f32> {
    let light_clip = light_view_proj * vec4<f32>(world_position, 1.0);
    let ndc = light_clip.xyz / light_clip.w;
    // NDC 的 xy 在 [-1,1]，纹理坐标在 [0,1]，且 y 轴方向相反。
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return vec3<f32>(uv, ndc.z);
}

// 按到相机的距离选级联。
//
// `splits` 的 x/y/z 是前三级的远距离，w 是级数。
// 返回的层号已经夹在有效范围内——越界会采到未初始化的层，那是一片噪点。
fn pick_cascade(view_depth: f32, splits: vec4<f32>) -> i32 {
    let count = i32(splits.w);
    if (view_depth < splits.x) { return 0; }
    if (count > 1 && view_depth < splits.y) { return 1; }
    if (count > 2 && view_depth < splits.z) { return 2; }
    return max(count - 1, 0);
}

// 软阴影：从级联数组的某一层采样，返回受光比例（1 = 完全受光）。
//
// `penumbra_ratio` 是**光源对着色点张开的半角的正切**（半尺寸 / 到光源
// 的距离）。给 0 就是固定的 3×3 PCF，也就是这个函数原来的行为——
// 方向光、点光源、聚光灯都走这一条，加这个功能不改它们的画面。
//
// 大于 0 时走 PCSS：先找遮挡物有多远，半影按「遮挡物到接收面的距离 ×
// 张角」张开。这正是面光源的定义性特征——**影子离遮挡物越远越糊**，
// 而固定半径的 PCF 无论怎么调都只能给出一圈等宽的模糊边。
fn shadow_factor_cascade(
    shadow_map: texture_depth_2d_array,
    shadow_sampler: sampler_comparison,
    light_view_proj: mat4x4<f32>,
    layer: i32,
    world_position: vec3<f32>,
    n_dot_l: f32,
    depth_bias: f32,
    resolution: f32,
    penumbra_ratio: f32,
) -> f32 {
    let projected = shadow_project(light_view_proj, world_position);
    let uv = projected.xy;
    let depth = projected.z;

    // 超出阴影贴图覆盖范围的地方一律视为受光，否则场景边缘会出现整块黑影。
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || depth > 1.0) {
        return 1.0;
    }

    // ── 偏移换算到归一化深度 ──
    //
    // `depth_bias` 是**世界单位（米）**。直接当归一化深度用的话，
    // 它的实际大小会随级联的深度范围漂移——而那个范围又随场景大小走。
    // 症状：把地面从 10×10 换成 100×100，角色的脚和小腿的阴影就没了，
    // 因为偏移从 2 厘米涨到了 22 厘米。
    //
    // 光空间矩阵的第三行 = 光的前向方向 / (far-near)，这一行的空间分量
    // 长度就是 1/(far-near)，取反得到深度范围。从矩阵里取而不是再传一个
    // uniform：这样级联各自的范围天然是对的。
    //
    // 注意不能只取 `m[2][2]`：它只含前向方向的 z 分量，光斜射时会把范围
    // 放大 1/|方向.z| 倍，偏移跟着变小——斜光下阴影痤疮更重；正下方向的光
    // （方向.z = 0）甚至会把范围算成无穷，偏移直接归零。
    let light_z_row = vec3<f32>(
        light_view_proj[0][2],
        light_view_proj[1][2],
        light_view_proj[2][2],
    );
    let depth_range = 1.0 / max(length(light_z_row), 1e-9);

    // 掠射角下同一个纹素跨越的深度差更大，偏移需要随之放大。
    let slope = clamp(1.0 - n_dot_l, 0.0, 1.0);
    let bias = (depth_bias / depth_range) * (1.0 + slope * 4.0);
    let compare = depth - bias;

    let texel = 1.0 / max(resolution, 1.0);

    // ── 半影有多宽 ──
    //
    // 张角为 0（方向光那些）时半径恒为一个纹素，退化成原来的 3×3 PCF。
    var radius_texels = 1.0;
    if (penumbra_ratio > 0.0) {
        // 一个世界单位有多少纹素。和上面的深度范围一样，从矩阵里取
        // 而不是再传一个 uniform——这样各级级联天然是对的。
        //
        // 正交矩阵第一列作为线性型的长度是 2/覆盖宽度（裁剪空间 x 跨越
        // 的是 [-1,1]），UV 跨越的是 1，所以除以 2。
        let x_row = vec3<f32>(
            light_view_proj[0][0],
            light_view_proj[1][0],
            light_view_proj[2][0],
        );
        let texels_per_world = length(x_row) * 0.5 * max(resolution, 1.0);

        // 1. 找遮挡物：在一小片区域里取原始深度，只统计比接收面更近的。
        //
        // 用 `textureLoad` 而不是比较采样——比较采样只会告诉你「通过没
        // 通过」，而这里要的是**遮挡物有多远**。
        let size = vec2<f32>(textureDimensions(shadow_map).xy);
        // 搜索半径按最大半影估：假设遮挡物贴着光源那一侧，
        // 也就是整段深度范围都可能是空的。夹一个上限免得代价失控。
        let search = clamp(penumbra_ratio * depth_range * texels_per_world * 0.25, 1.0, 12.0);
        var blocker_sum = 0.0;
        var blocker_count = 0.0;
        for (var y = -2; y <= 2; y = y + 1) {
            for (var x = -2; x <= 2; x = x + 1) {
                let offset = vec2<f32>(f32(x), f32(y)) * (search * 0.5);
                let coord = vec2<i32>(clamp(
                    uv * size + offset,
                    vec2<f32>(0.0),
                    size - vec2<f32>(1.0),
                ));
                let sample = textureLoad(shadow_map, coord, layer, 0);
                if (sample < compare) {
                    blocker_sum += sample;
                    blocker_count += 1.0;
                }
            }
        }

        // 一个遮挡物都没有：完全受光。提前返回也省掉后面那 25 次采样。
        if (blocker_count < 0.5) {
            return 1.0;
        }

        // 2. 半影宽度 = 遮挡物到接收面的距离 × 张角。
        let blocker = blocker_sum / blocker_count;
        let separation = max(compare - blocker, 0.0) * depth_range;
        radius_texels = clamp(separation * penumbra_ratio * texels_per_world, 1.0, 16.0);
    }

    // ── PCF ──
    //
    // 5×5 而不是 3×3：半径大起来之后 3×3 的九个点会散开成看得见的九块，
    // 表现为影子边缘的条带。
    let step = radius_texels * texel * 0.5;
    var sum = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * step;
            // 硬件比较采样：一次返回该纹素通过深度测试的比例。
            sum += textureSampleCompare(
                shadow_map, shadow_sampler, uv + offset, layer, compare
            );
        }
    }

    return sum / 25.0;
}
