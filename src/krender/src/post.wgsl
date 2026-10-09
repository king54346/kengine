// 后处理：Bloom 与色调映射。
//
// 几个 pass 共用这份代码，靠不同的入口点区分：
//   bloom_extract_fs  提取亮部，同时降到半分辨率（mip 0）
//   bloom_down_fs     mip i-1 → mip i，13 抽头降采样
//   bloom_up_fs       mip i+1 → mip i，3×3 帐篷滤波，加法混合叠上去
//   composite_fs      合成 Bloom 并做色调映射，输出到屏幕
//
// # 为什么是一条 mip 链而不是一次高斯
//
// 一次 9 抽头高斯（这里原来的做法）只能糊开几个像素：想要更大的光晕
// 就得加抽头或者多糊几遍，开销线性涨。降采样链每往下一级范围翻倍、
// 像素数变四分之一，六级下来光晕能铺满半个屏幕，总开销还不到
// 一次全分辨率的全屏 pass。这也是 Unreal、COD、three.js 的
// UnrealBloomPass 的做法。
//
// 链有了，「光晕多大」就变成一个连续可调的量（`radius`）：
// 往上合的时候每一级乘多少。

struct PostParams {
    // x = Bloom 阈值，y = Bloom 强度，z = 色调映射算子编号，w = 曝光
    settings: vec4<f32>,
    // xy = 当前采样纹理的纹素尺寸，z = 软阈值宽度，w 保留
    texel: vec4<f32>,
    // x = 遮罩通道（<0 表示不用遮罩），y = 升采样输出的倍数（最小那一级的权重），
    // z = 各级权重之和（合成时除掉），w 保留
    extra: vec4<f32>,
    padding: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: PostParams;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;
@group(0) @binding(3) var mask_texture: texture_2d<f32>;
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

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn sample_source(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(source, source_sampler, uv, 0.0).rgb;
}

// ── 亮部提取 ──

@fragment
fn bloom_extract_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    // 输出是半分辨率：四个双线性采样各盖住源图的 2×2，合起来是 4×4 的
    // 盒式滤波。直接取一次的话，只有四分之一的源像素参与，细小的高光
    // 在相机移动时会一闪一闪。
    let t = params.texel.xy;
    var color = sample_source(in.uv + vec2<f32>(-t.x, -t.y))
        + sample_source(in.uv + vec2<f32>(t.x, -t.y))
        + sample_source(in.uv + vec2<f32>(-t.x, t.y))
        + sample_source(in.uv + vec2<f32>(t.x, t.y));
    color *= 0.25;
    // 半精度的上限附近会出 inf，一个 inf 像素经过模糊会把整个光晕变成白屏。
    color = min(color, vec3<f32>(60000.0));

    // 用感知亮度而非平均值，避免纯蓝等低亮度饱和色被误判为高光。
    let lum = luminance(color);
    let threshold = params.settings.x;
    let knee = max(params.texel.z, 1e-4);
    // 软阈值（二次曲线过渡）：硬切会让 Bloom 边界出现明显的轮廓。
    let soft = clamp(lum - threshold + knee, 0.0, 2.0 * knee);
    let soft_curve = soft * soft / (4.0 * knee + 1e-4);
    let contribution = max(soft_curve, lum - threshold) / max(lum, 1e-4);
    var result = color * max(contribution, 0.0);

    // 选择性辉光：只有遮罩通道里「看得见」的像素才发光。
    let channel = i32(params.extra.x);
    if (channel >= 0) {
        let mask = textureSampleLevel(mask_texture, source_sampler, in.uv, 0.0)[channel];
        result *= step(0.75, mask);
    }

    return vec4<f32>(result, 1.0);
}

// ── 降采样：COD 的 13 抽头 ──
//
// 五个重叠的 2×2 盒子加权平均。比单个双线性采样多八次读取，换来的是
// 降采样时不丢高频——少了它，细亮线在一级级往下走的时候会时有时无。

@fragment
fn bloom_down_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let t = params.texel.xy;
    let a = sample_source(in.uv + t * vec2<f32>(-2.0, -2.0));
    let b = sample_source(in.uv + t * vec2<f32>(0.0, -2.0));
    let c = sample_source(in.uv + t * vec2<f32>(2.0, -2.0));
    let d = sample_source(in.uv + t * vec2<f32>(-2.0, 0.0));
    let e = sample_source(in.uv);
    let f = sample_source(in.uv + t * vec2<f32>(2.0, 0.0));
    let g = sample_source(in.uv + t * vec2<f32>(-2.0, 2.0));
    let h = sample_source(in.uv + t * vec2<f32>(0.0, 2.0));
    let i = sample_source(in.uv + t * vec2<f32>(2.0, 2.0));
    let j = sample_source(in.uv + t * vec2<f32>(-1.0, -1.0));
    let k = sample_source(in.uv + t * vec2<f32>(1.0, -1.0));
    let l = sample_source(in.uv + t * vec2<f32>(-1.0, 1.0));
    let m = sample_source(in.uv + t * vec2<f32>(1.0, 1.0));

    var color = e * 0.125;
    color += (a + c + g + i) * 0.03125;
    color += (b + d + f + h) * 0.0625;
    color += (j + k + l + m) * 0.125;
    return vec4<f32>(color, 1.0);
}

// ── 升采样：3×3 帐篷 ──
//
// 结果**加**到目标那一级上；目标原有的内容由混合常数乘上它自己的权重
// （见 `bloom_level_weights`）。权重由 `radius` 决定：越大，越宽的级别
// 占的越多，光晕越大。

@fragment
fn bloom_up_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let t = params.texel.xy;
    var color = sample_source(in.uv) * 4.0;
    color += (sample_source(in.uv + vec2<f32>(-t.x, 0.0))
        + sample_source(in.uv + vec2<f32>(t.x, 0.0))
        + sample_source(in.uv + vec2<f32>(0.0, -t.y))
        + sample_source(in.uv + vec2<f32>(0.0, t.y))) * 2.0;
    color += sample_source(in.uv + vec2<f32>(-t.x, -t.y))
        + sample_source(in.uv + vec2<f32>(t.x, -t.y))
        + sample_source(in.uv + vec2<f32>(-t.x, t.y))
        + sample_source(in.uv + vec2<f32>(t.x, t.y));
    color *= 1.0 / 16.0;
    return vec4<f32>(color * params.extra.y, 1.0);
}

// ── 色调映射 ──
//
// 和 `tonemap.rs` 的 CPU 版本一一对应，曲线的性质在那边断言。

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

fn agx_contrast(x: vec3<f32>) -> vec3<f32> {
    let x2 = x * x;
    let x4 = x2 * x2;
    return 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
}

fn tonemap_agx(input: vec3<f32>) -> vec3<f32> {
    let srgb_to_rec2020 = mat3x3<f32>(
        vec3<f32>(0.6274, 0.0691, 0.0164),
        vec3<f32>(0.3293, 0.9195, 0.0880),
        vec3<f32>(0.0433, 0.0113, 0.8956),
    );
    let rec2020_to_srgb = mat3x3<f32>(
        vec3<f32>(1.6605, -0.1246, -0.0182),
        vec3<f32>(-0.5876, 1.1329, -0.1006),
        vec3<f32>(-0.0728, -0.0083, 1.1187),
    );
    let inset = mat3x3<f32>(
        vec3<f32>(0.85662715, 0.13731897, 0.11189821),
        vec3<f32>(0.09512124, 0.76124199, 0.07679942),
        vec3<f32>(0.04825161, 0.10143904, 0.81130237),
    );
    let outset = mat3x3<f32>(
        vec3<f32>(1.1271006, -0.14132976, -0.14132976),
        vec3<f32>(-0.11060664, 1.1578237, -0.11060664),
        vec3<f32>(-0.01649394, -0.01649394, 1.2519364),
    );
    let min_ev = -12.473931;
    let max_ev = 4.026069;

    var color = inset * (srgb_to_rec2020 * input);
    color = clamp(color, vec3<f32>(1e-10), vec3<f32>(65504.0));
    color = clamp((log2(color) - min_ev) / (max_ev - min_ev), vec3<f32>(0.0), vec3<f32>(1.0));
    color = agx_contrast(color);
    color = pow(max(outset * color, vec3<f32>(0.0)), vec3<f32>(2.2));
    return clamp(rec2020_to_srgb * color, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn tonemap_neutral(input: vec3<f32>) -> vec3<f32> {
    let start = 0.8 - 0.04;
    let desaturation = 0.15;
    var color = min(input, vec3<f32>(65504.0));
    let x = min(color.r, min(color.g, color.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    color -= offset;
    let peak = max(color.r, max(color.g, color.b));
    if (peak < start) {
        return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    color *= new_peak / peak;
    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return clamp(mix(color, vec3<f32>(new_peak), g), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn tonemap_cineon(input: vec3<f32>) -> vec3<f32> {
    let color = max(min(input, vec3<f32>(65504.0)) - 0.004, vec3<f32>(0.0));
    let mapped = (color * (6.2 * color + 0.5)) / (color * (6.2 * color + 1.7) + 0.06);
    return clamp(pow(mapped, vec3<f32>(2.2)), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn tonemap(color: vec3<f32>, mode: u32) -> vec3<f32> {
    let positive = max(color, vec3<f32>(0.0));
    switch mode {
        case 1u: { return tonemap_reinhard(positive); }
        case 2u: { return tonemap_aces(positive); }
        case 3u: { return tonemap_agx(positive); }
        case 4u: { return tonemap_neutral(positive); }
        case 5u: { return tonemap_cineon(positive); }
        default: { return clamp(positive, vec3<f32>(0.0), vec3<f32>(1.0)); }
    }
}

// ── 合成 ──

@fragment
fn composite_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    var color = sample_source(in.uv);
    // 链上每一级都叠进了 mip 0，除掉权重之和，强度的意义才不随级数、
    // 半径变化：「强度 1」始终是「亮部原样加一份」。
    let glow = textureSampleLevel(bloom, bloom_sampler, in.uv, 0.0).rgb;
    color += glow * params.settings.y / max(params.extra.z, 1e-4);

    // 曝光乘在色调映射**之前**：乘在之后等于把压好的曲线整体拉伸，
    // 高光会重新超出 1 再被硬切掉。
    color = color * params.settings.w;
    color = tonemap(color, u32(params.settings.z));

    // 这里不做 gamma 校正：交换链是 sRGB 格式，由硬件负责转换。
    return vec4<f32>(color, 1.0);
}
