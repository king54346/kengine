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

// 3×3 PCF 软阴影。返回受光比例：1 表示完全受光，0 表示完全在阴影里。
fn shadow_factor(
    shadow_map: texture_depth_2d,
    shadow_sampler: sampler_comparison,
    light_view_proj: mat4x4<f32>,
    world_position: vec3<f32>,
    n_dot_l: f32,
    depth_bias: f32,
    resolution: f32,
) -> f32 {
    let projected = shadow_project(light_view_proj, world_position);
    let uv = projected.xy;
    let depth = projected.z;

    // 超出阴影贴图覆盖范围的地方一律视为受光，否则场景边缘会出现整块黑影。
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || depth > 1.0) {
        return 1.0;
    }

    // 掠射角下同一个纹素跨越的深度差更大，偏移需要随之放大。
    let slope = clamp(1.0 - n_dot_l, 0.0, 1.0);
    let bias = depth_bias * (1.0 + slope * 4.0);
    let compare = depth - bias;

    let texel = 1.0 / max(resolution, 1.0);
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            // 硬件比较采样：一次返回该纹素通过深度测试的比例。
            sum += textureSampleCompare(shadow_map, shadow_sampler, uv + offset, compare);
        }
    }

    return sum / 9.0;
}
