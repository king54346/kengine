// AMD FidelityFX Super Resolution 1（FSR1）：低分辨率画面放大到屏幕分辨率。两个 pass：
//
// - EASU（Edge Adaptive Spatial Upsampling）：每个输出像素取输入的 12 个纹素，按局部亮度梯度估计边缘方向和
//   「有多像一条边」，把一个 Lanczos-2 近似核沿边方向拉长、垂直方向压扁再加权——边缘放大以后还是锐的，
//   不像双线性那样糊。最后夹在最近 4 个纹素的最小 / 最大值之间，防振铃。
// - RCAS（Robust Contrast Adaptive Sharpening）：在输出分辨率上做一次「不会过冲」的锐化：
//   按邻域的最小 / 最大值算出最多能锐化多少而不溢出，再按 `sharpness` 衰减。
//
// 按 FidelityFX 的 ffx_fsr1.h（MIT）里的标量版本移植，略去了半精度和打包的优化。

struct Params {
    // 输入（低分辨率）尺寸。
    input_size: vec2<f32>,
    // 输出（屏幕）尺寸。
    output_size: vec2<f32>,
    // RCAS 的锐化量：0 最锐，每加 1 锐化减半（和 FSR 的「stops」一样）。
    sharpness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct FullscreenOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn fsr_vs(@builtin(vertex_index) index: u32) -> FullscreenOutput {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: FullscreenOutput;
    out.clip_position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn load(p: vec2<i32>) -> vec3<f32> {
    let size = vec2<i32>(textureDimensions(source));
    return textureLoad(source, clamp(p, vec2<i32>(0), size - 1), 0).rgb;
}

// FSR 用的近似亮度：B·0.5 + (R·0.5 + G)。只用来找边缘方向，比例不要紧。
fn luma(c: vec3<f32>) -> f32 {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
}

// 累积一个 2×2 双线性格点上的方向和边缘长度（FsrEasuSetF）。a 上、b 左、c 中、d 右、e 下。
fn easu_set(dir: ptr<function, vec2<f32>>, len: ptr<function, f32>, w: f32, a: f32, b: f32, c: f32, d: f32, e: f32) {
    let dc = d - c;
    let cb = c - b;
    var len_x = max(abs(dc), abs(cb));
    len_x = select(1.0 / len_x, 0.0, len_x == 0.0);
    let dir_x = d - b;
    len_x = clamp(abs(dir_x) * len_x, 0.0, 1.0);
    len_x *= len_x;
    let ec = e - c;
    let ca = c - a;
    var len_y = max(abs(ec), abs(ca));
    len_y = select(1.0 / len_y, 0.0, len_y == 0.0);
    let dir_y = e - a;
    len_y = clamp(abs(dir_y) * len_y, 0.0, 1.0);
    len_y *= len_y;
    *dir += vec2<f32>(dir_x, dir_y) * w;
    *len += (len_x + len_y) * w;
}

// 一个纹素的贡献（FsrEasuTapF）：偏移旋到边缘方向上、按各向异性缩放，Lanczos-2 的多项式近似。
fn easu_tap(color_sum: ptr<function, vec3<f32>>, weight_sum: ptr<function, f32>, offset: vec2<f32>, dir: vec2<f32>, len2: vec2<f32>, lobe: f32, clip: f32, color: vec3<f32>) {
    var v = vec2<f32>(offset.x * dir.x + offset.y * dir.y, offset.x * -dir.y + offset.y * dir.x);
    v *= len2;
    let d2 = min(dot(v, v), clip);
    var wb = 2.0 / 5.0 * d2 - 1.0;
    var wa = lobe * d2 - 1.0;
    wb *= wb;
    wa *= wa;
    wb = 25.0 / 16.0 * wb - (25.0 / 16.0 - 1.0);
    let w = wb * wa;
    *color_sum += color * w;
    *weight_sum += w;
}

@fragment
fn easu_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    // 输出像素中心映射到输入的纹素坐标（以纹素中心为整数点）。
    let pp_full = in.uv * params.input_size - 0.5;
    let fp = floor(pp_full);
    let pp = pp_full - fp;
    let o = vec2<i32>(fp);
    //    b c
    //  e f g h
    //  i j k l
    //    n o
    let b = load(o + vec2<i32>(0, -1));
    let c = load(o + vec2<i32>(1, -1));
    let e = load(o + vec2<i32>(-1, 0));
    let f = load(o + vec2<i32>(0, 0));
    let g = load(o + vec2<i32>(1, 0));
    let h = load(o + vec2<i32>(2, 0));
    let i = load(o + vec2<i32>(-1, 1));
    let j = load(o + vec2<i32>(0, 1));
    let k = load(o + vec2<i32>(1, 1));
    let l = load(o + vec2<i32>(2, 1));
    let n = load(o + vec2<i32>(0, 2));
    let p = load(o + vec2<i32>(1, 2));
    let bl = luma(b);
    let cl = luma(c);
    let el = luma(e);
    let fl = luma(f);
    let gl = luma(g);
    let hl = luma(h);
    let il = luma(i);
    let jl = luma(j);
    let kl = luma(k);
    let ll = luma(l);
    let nl = luma(n);
    let pl = luma(p);

    var dir = vec2<f32>(0.0);
    var len = 0.0;
    easu_set(&dir, &len, (1.0 - pp.x) * (1.0 - pp.y), bl, el, fl, gl, jl);
    easu_set(&dir, &len, pp.x * (1.0 - pp.y), cl, fl, gl, hl, kl);
    easu_set(&dir, &len, (1.0 - pp.x) * pp.y, fl, il, jl, kl, nl);
    easu_set(&dir, &len, pp.x * pp.y, gl, jl, kl, ll, pl);

    // 方向归一化；几乎没有梯度时退回水平方向（核是各向同性的，方向无所谓）。
    let dir2 = dir * dir;
    var dir_r = dir2.x + dir2.y;
    let zero = dir_r < 1.0 / 32768.0;
    dir_r = select(inverseSqrt(dir_r), 1.0, zero);
    dir = select(dir, vec2<f32>(1.0, 0.0), zero) * dir_r;
    len = len * 0.5;
    len *= len;
    // 斜向的边要多拉长一点（单位方向在方形格子上的投影）。
    let stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    let len2 = vec2<f32>(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lobe = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    let clip = 1.0 / lobe;

    var color_sum = vec3<f32>(0.0);
    var weight_sum = 0.0;
    easu_tap(&color_sum, &weight_sum, vec2<f32>(0.0, -1.0) - pp, dir, len2, lobe, clip, b);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(1.0, -1.0) - pp, dir, len2, lobe, clip, c);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(-1.0, 1.0) - pp, dir, len2, lobe, clip, i);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(0.0, 1.0) - pp, dir, len2, lobe, clip, j);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(0.0, 0.0) - pp, dir, len2, lobe, clip, f);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(-1.0, 0.0) - pp, dir, len2, lobe, clip, e);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(1.0, 1.0) - pp, dir, len2, lobe, clip, k);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(2.0, 1.0) - pp, dir, len2, lobe, clip, l);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(2.0, 0.0) - pp, dir, len2, lobe, clip, h);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(1.0, 0.0) - pp, dir, len2, lobe, clip, g);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(1.0, 2.0) - pp, dir, len2, lobe, clip, p);
    easu_tap(&color_sum, &weight_sum, vec2<f32>(0.0, 2.0) - pp, dir, len2, lobe, clip, n);

    // 防振铃：夹在最近四个纹素之间。
    let lo = min(min(f, g), min(j, k));
    let hi = max(max(f, g), max(j, k));
    let color = clamp(color_sum / max(weight_sum, 1e-5), lo, hi);
    return vec4<f32>(color, 1.0);
}

@fragment
fn rcas_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.clip_position.xy);
    //   b
    // d e f
    //   h
    let b = load(p + vec2<i32>(0, -1));
    let d = load(p + vec2<i32>(-1, 0));
    let e = load(p);
    let f = load(p + vec2<i32>(1, 0));
    let h = load(p + vec2<i32>(0, 1));
    let mn = min(min(b, d), min(f, h));
    let mx = max(max(b, d), max(f, h));
    // 锐化的「瓣」能多负而不让结果溢出 [0, 1]。
    let hit_min = mn / max(4.0 * mx, vec3<f32>(1e-5));
    let hit_max = (1.0 - mx) / min(4.0 * mn - 4.0, vec3<f32>(-1e-5));
    let lobe_rgb = max(-hit_min, hit_max);
    let limit = 0.25 - 1.0 / 16.0;
    let lobe = max(-limit, min(max(lobe_rgb.r, max(lobe_rgb.g, lobe_rgb.b)), 0.0)) * exp2(-params.sharpness);
    let color = (lobe * (b + d + f + h) + e) / (4.0 * lobe + 1.0);
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
