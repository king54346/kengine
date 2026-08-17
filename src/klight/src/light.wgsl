// klight —— 光源求值的 WGSL 实现。
//
// 由渲染器拼接到标准着色器前面。每个函数在 Rust 侧都有等价实现，
// 两者必须保持一致——Rust 那份用于在 CPU 上断言衰减曲线的性质。

// 与 Rust 侧的 GpuLight 逐字段对应，改动需同步两边。
struct Light {
    // xyz = 世界坐标（方向光未使用），w = 类型：0 方向光 / 1 点光源 / 2 聚光灯
    position: vec4<f32>,
    // xyz = 光线传播方向（方向光与聚光灯使用），w = 作用半径
    direction: vec4<f32>,
    // rgb = 颜色，a = 强度
    color: vec4<f32>,
    // x = 内锥余弦，y = 外锥余弦，zw 保留
    params: vec4<f32>,
};

const LIGHT_DIRECTIONAL: f32 = 0.0;
const LIGHT_POINT: f32 = 1.0;
const LIGHT_SPOT: f32 = 2.0;

// 距离衰减：物理上的平方反比，再乘一个窗函数在 range 处平滑归零。
//
// 纯 1/d² 永远不会真正到 0，会导致远处光源仍需参与计算；
// 窗函数让光源有明确的作用范围，便于后续做光源剔除。
fn light_distance_attenuation(distance: f32, range: f32) -> f32 {
    if (range <= 0.0) {
        return 0.0;
    }
    // 分母加 1 避免距离趋零时除爆。
    let falloff = 1.0 / (1.0 + distance * distance);

    let ratio = clamp(distance / range, 0.0, 1.0);
    let ratio2 = ratio * ratio;
    let window = clamp(1.0 - ratio2 * ratio2, 0.0, 1.0);

    return falloff * window * window;
}

// 聚光灯的锥形衰减：内锥内为 1，内外锥之间平滑过渡，外锥外为 0。
fn light_spot_attenuation(cos_angle: f32, cos_inner: f32, cos_outer: f32) -> f32 {
    // 内外锥重合时不能除零，退化为硬边缘。
    let denominator = cos_inner - cos_outer;
    if (denominator <= 1e-5) {
        return select(0.0, 1.0, cos_angle >= cos_outer);
    }
    let t = clamp((cos_angle - cos_outer) / denominator, 0.0, 1.0);
    // 平方一下让边缘过渡更柔和。
    return t * t;
}

// 求某个光源在给定着色点上的入射方向与辐射亮度。
// 返回 xyz = 指向光源的单位向量，w = 该方向上的衰减系数。
fn light_sample_direction(light: Light, world_position: vec3<f32>) -> vec4<f32> {
    if (light.position.w == LIGHT_DIRECTIONAL) {
        // 方向光没有位置，也不衰减。
        return vec4<f32>(normalize(-light.direction.xyz), 1.0);
    }

    let to_light = light.position.xyz - world_position;
    let distance = length(to_light);
    if (distance <= 1e-5) {
        return vec4<f32>(0.0, 1.0, 0.0, 0.0);
    }
    let l = to_light / distance;

    var attenuation = light_distance_attenuation(distance, light.direction.w);

    if (light.position.w == LIGHT_SPOT) {
        // 着色点相对聚光灯轴线的夹角余弦。
        let cos_angle = dot(normalize(light.direction.xyz), -l);
        attenuation *= light_spot_attenuation(cos_angle, light.params.x, light.params.y);
    }

    return vec4<f32>(l, attenuation);
}

// 光源的辐射亮度（颜色 × 强度 × 衰减）。
fn light_radiance(light: Light, attenuation: f32) -> vec3<f32> {
    return light.color.rgb * light.color.a * attenuation;
}
