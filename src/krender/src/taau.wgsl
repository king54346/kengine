// 时间性放大（TAAU）：低分辨率、每帧抖动一个亚像素的画面，累积进一张**屏幕分辨率**的历史图。
//
// 和普通 TAA 的区别在历史图的分辨率：TAA 在渲染分辨率上累积，只抗锯齿；TAAU 在输出分辨率上累积，
// 每帧的低分辨率样本落在输出像素网格的不同位置（抖动），几帧下来就攒出了比渲染分辨率高的细节。
//
// 每个输出像素：
// 1. 本帧：取它周围 3×3 个低分辨率纹素，按「纹素实际采样的位置（纹素中心 − 抖动）到这个输出像素中心的距离」
//    做高斯加权（Karis 2014 的 TAAU 做法）。同时记下邻域在 YCoCg 里的最小 / 最大值。
// 2. 历史：按运动向量回到上一帧的位置采样（双线性），夹进邻域的包围盒里防残影。
// 3. 混合：离本像素中心最近的那个样本越近，越信本帧（这一帧正好采到了这里）。

struct Params {
    // 渲染（低分辨率）尺寸。
    input_size: vec2<f32>,
    // 输出（屏幕）尺寸。
    output_size: vec2<f32>,
    // 本帧的抖动，单位是低分辨率像素（x 向右、y 向上，和投影里的平移同向）。
    jitter: vec2<f32>,
    // > 0.5：历史可用（第一帧、改尺寸之后为 0）。
    history_valid: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var history: texture_2d<f32>;
@group(0) @binding(4) var velocity: texture_2d<f32>;

struct FullscreenOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn taau_vs(@builtin(vertex_index) index: u32) -> FullscreenOutput {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: FullscreenOutput;
    out.clip_position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn rgb_to_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(0.25 * c.r + 0.5 * c.g + 0.25 * c.b, 0.5 * c.r - 0.5 * c.b, -0.25 * c.r + 0.5 * c.g - 0.25 * c.b);
}

fn ycocg_to_rgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
}

@fragment
fn taau_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    // 这个输出像素中心在低分辨率像素坐标里的位置（不带抖动的那个网格）。
    let p = in.uv * params.input_size;
    let n = vec2<i32>(floor(p));
    // 纹素 t 的中心采到的是场景里 t + 0.5 − 抖动 的位置（屏幕 y 向下，抖动的 y 向上，所以 y 取 +）。
    let shift = vec2<f32>(-params.jitter.x, params.jitter.y);

    var sum = vec3<f32>(0.0);
    var weight_sum = 0.0;
    var closest = 0.0;
    var low = vec3<f32>(1e9);
    var high = vec3<f32>(-1e9);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let t = clamp(n + vec2<i32>(x, y), vec2<i32>(0), size - 1);
            let color = textureLoad(source, t, 0).rgb;
            let d = vec2<f32>(t) + 0.5 + shift - p;
            let w = exp(-2.29 * dot(d, d));
            sum += color * w;
            weight_sum += w;
            closest = max(closest, w);
            let ycocg = rgb_to_ycocg(color);
            low = min(low, ycocg);
            high = max(high, ycocg);
        }
    }
    let current = sum / max(weight_sum, 1e-5);

    // 运动向量存的是 NDC 位移，换成 UV（y 翻向）——和 TAA 的 `velocity_uv` 一样。
    let motion = textureLoad(velocity, clamp(n, vec2<i32>(0), vec2<i32>(textureDimensions(velocity)) - 1), 0).xy * vec2<f32>(0.5, -0.5);
    let previous_uv = in.uv - motion;
    let outside = any(previous_uv < vec2<f32>(0.0)) || any(previous_uv > vec2<f32>(1.0));
    if (params.history_valid < 0.5 || outside) {
        return vec4<f32>(current, 1.0);
    }
    var past = textureSampleLevel(history, linear_sampler, previous_uv, 0.0).rgb;
    past = ycocg_to_rgb(clamp(rgb_to_ycocg(past), low, high));
    // 最近的样本正好落在像素中心（权重 1）时本帧占四成，离得远时只占几个百分点。
    let blend = mix(0.04, 0.4, closest);
    return vec4<f32>(mix(past, current, blend), 1.0);
}
