// 后处理：Bloom 与色调映射。
//
// 三个 pass 共用这份代码，靠不同的入口点区分：
//   bloom_extract_fs  提取亮部
//   bloom_blur_fs     一维高斯模糊（水平/垂直各跑一次）
//   composite_fs      合成 Bloom 并做色调映射，输出到屏幕

struct PostParams {
    // x = Bloom 阈值，y = Bloom 强度，z = 色调映射算子编号，w 保留
    settings: vec4<f32>,
    // xy = 当前采样纹理的纹素尺寸，zw = 模糊方向
    texel: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: PostParams;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;
@group(1) @binding(0) var bloom: texture_2d<f32>;
@group(1) @binding(1) var bloom_sampler: sampler;

struct FullscreenOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 覆盖全屏的单个三角形，比两个三角形少一条对角线上的重复着色。
@vertex
fn fullscreen_vs(@builtin(vertex_index) index: u32) -> FullscreenOutput {
    let ndc = vec2<f32>(
        f32((index << 1u) & 2u) * 2.0 - 1.0,
        f32(index & 2u) * 2.0 - 1.0,
    );

    var out: FullscreenOutput;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    // NDC 的 y 向上，纹理坐标的 y 向下。
    out.uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

// ── 亮部提取 ──

@fragment
fn bloom_extract_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let color = textureSample(source, source_sampler, in.uv).rgb;

    // 用感知亮度而非平均值，避免纯蓝等低亮度饱和色被误判为高光。
    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let threshold = params.settings.x;

    // 软阈值：在阈值附近平滑过渡，硬切会让 Bloom 边界出现明显的轮廓。
    let contribution = max(luminance - threshold, 0.0) / max(luminance, 1e-4);

    return vec4<f32>(color * contribution, 1.0);
}

// ── 一维高斯模糊 ──

@fragment
fn bloom_blur_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    // 归一化的 9 抽头高斯权重（σ≈2）。
    let weights = array<f32, 5>(0.227027, 0.1945946, 0.1216216, 0.054054, 0.016216);
    let step = params.texel.xy * params.texel.zw;

    var result = textureSample(source, source_sampler, in.uv).rgb * weights[0];
    for (var i = 1; i < 5; i = i + 1) {
        let offset = step * f32(i);
        result += textureSample(source, source_sampler, in.uv + offset).rgb * weights[i];
        result += textureSample(source, source_sampler, in.uv - offset).rgb * weights[i];
    }

    return vec4<f32>(result, 1.0);
}

// ── 色调映射 ──

fn tonemap_reinhard(color: vec3<f32>) -> vec3<f32> {
    return color / (1.0 + color);
}

fn tonemap_aces(color: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((color * (a * color + b)) / (color * (c * color + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn tonemap(color: vec3<f32>, mode: u32) -> vec3<f32> {
    let positive = max(color, vec3<f32>(0.0));
    if (mode == 1u) {
        return tonemap_reinhard(positive);
    }
    if (mode == 2u) {
        return tonemap_aces(positive);
    }
    return clamp(positive, vec3<f32>(0.0), vec3<f32>(1.0));
}

// ── 合成 ──

@fragment
fn composite_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    var color = textureSample(source, source_sampler, in.uv).rgb;
    color += textureSample(bloom, bloom_sampler, in.uv).rgb * params.settings.y;

    color = tonemap(color, u32(params.settings.z));

    // 这里不做 gamma 校正：交换链是 sRGB 格式，由硬件负责转换。
    return vec4<f32>(color, 1.0);
}
