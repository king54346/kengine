// 一组单 pass 的小效果。每个效果只用到自己的那个入口和参数；
// 同一份源码编进不同的效果里，没用到的入口不会被编成管线。
//
// 参数都走 `params[i]` 而不是 `param_<名字>()`：这份源码被好几个效果共用，
// 而每个效果只声明了自己的那几个名字——用名字的话别的入口会引用到
// 没生成的函数，整份源码编不过。各入口开头注释了它的参数布局。

// ── 色差 ──
// params[0].x = 强度，params[1].xy = 中心（-1..1），params[2].x = 缩放
@fragment
fn chromatic_aberration(in: PostVertex) -> @location(0) vec4<f32> {
    let center = params[1].xy * 0.5 + vec2<f32>(0.5);
    let offset = (in.uv - center) * params[0].x * 0.02 * params[2].x;
    let r = sample_input(in.uv + offset).r;
    let g = sample_input(in.uv).g;
    let b = sample_input(in.uv - offset).b;
    return vec4<f32>(r, g, b, 1.0);
}

// ── Sobel 边缘 ──
// 无参数。输出灰度的梯度大小，和 three.js 的 SobelOperatorNode 一样。
@fragment
fn sobel(in: PostVertex) -> @location(0) vec4<f32> {
    let t = frame.resolution.zw;
    var values: array<f32, 9>;
    var index = 0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            values[index] = luminance(linear_to_srgb(sample_input(in.uv + vec2<f32>(f32(x), f32(y)) * t).rgb));
            index = index + 1;
        }
    }
    let gx = -values[0] - 2.0 * values[3] - values[6] + values[2] + 2.0 * values[5] + values[8];
    let gy = -values[0] - 2.0 * values[1] - values[2] + values[6] + 2.0 * values[7] + values[8];
    let magnitude = srgb_to_linear(vec3<f32>(sqrt(gx * gx + gy * gy)));
    return vec4<f32>(magnitude, 1.0);
}

// ── 3D LUT（条带图）──
// params[0].x = 格子数 N，params[1].x = 强度。user0 = N² × N 的条带：
// x = r + b·N，y = g。在显示空间（sRGB）里查——调色 LUT 都是按那个空间做的。
fn lut_fetch(color: vec3<f32>, size: f32) -> vec3<f32> {
    let scaled = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)) * (size - 1.0);
    let slice0 = floor(scaled.b);
    let slice1 = min(slice0 + 1.0, size - 1.0);
    let fraction = scaled.b - slice0;
    let width = size * size;
    // 在一片里做双线性：采样点落在格子中心（+0.5），而且不跨片。
    let xy = vec2<f32>(scaled.r + 0.5, scaled.g + 0.5);
    let uv0 = vec2<f32>((xy.x + slice0 * size) / width, xy.y / size);
    let uv1 = vec2<f32>((xy.x + slice1 * size) / width, xy.y / size);
    let a = textureSampleLevel(user0, linear_sampler, uv0, 0.0).rgb;
    let b = textureSampleLevel(user0, linear_sampler, uv1, 0.0).rgb;
    return mix(a, b, fraction);
}

@fragment
fn lut(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let display = linear_to_srgb(color.rgb);
    let graded = lut_fetch(display, max(params[0].x, 2.0));
    return vec4<f32>(srgb_to_linear(mix(display, graded, clamp(params[1].x, 0.0, 1.0))), color.a);
}

// ── 残影 ──
// params[0].x = 衰减（damp），和 three.js 的 AfterImageNode 一样：
// 旧的像素乘上衰减、低于 0.1 的直接丢掉，再和新的取 max。
@fragment
fn afterimage(in: PostVertex) -> @location(0) vec4<f32> {
    let current = sample_input(in.uv);
    if (!history_valid()) {
        return current;
    }
    var old = sample_history(in.uv);
    old = old * params[0].x * step(vec4<f32>(0.1), old);
    return max(current, old);
}

// ── 径向模糊 ──
// params[0].xy = 中心（UV），params[1] = (权重, 衰减, 采样数, 曝光)
@fragment
fn radial_blur(in: PostVertex) -> @location(0) vec4<f32> {
    let center = params[0].xy;
    let weight = params[1].x;
    let decay = params[1].y;
    let count = clamp(i32(params[1].z), 1, 128);
    let exposure = params[1].w;
    let base = sample_input(in.uv).rgb;
    let step_uv = (center - in.uv) / f32(count);
    // 起点抖一下：采样数少的时候，等间距的步子会在画面上留下一圈圈条带。
    var uv = in.uv + step_uv * interleaved_gradient_noise(in.position.xy);
    var w = weight;
    var total = vec3<f32>(0.0);
    for (var i = 0; i < count; i = i + 1) {
        uv += step_uv;
        total += sample_input(uv).rgb * w;
        w *= decay;
    }
    let blur = total * exposure / f32(count);
    // 和 three.js 一样：模糊和原图各一半（原图乘 2，所以原图的亮度不变）。
    return vec4<f32>(mix(blur, base * 2.0, 0.5), 1.0);
}

// ── 网点 ──
// params[0].xy = 中心（UV），params[1] = (角度, 缩放, _, _)
@fragment
fn dot_screen(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let average = luminance(linear_to_srgb(color.rgb));
    let angle = params[1].x;
    let scale = params[1].y;
    let s = sin(angle);
    let c = cos(angle);
    let tex = (in.uv - params[0].xy) * frame.resolution.xy;
    let point = vec2<f32>(c * tex.x - s * tex.y, s * tex.x + c * tex.y) * scale;
    let pattern = (sin(point.x) * sin(point.y)) * 4.0;
    return vec4<f32>(srgb_to_linear(vec3<f32>(average * 10.0 - 5.0 + pattern)), color.a);
}

// ── RGB 错位 ──
// params[0] = (位移量, 角度, _, _)
@fragment
fn rgb_shift(in: PostVertex) -> @location(0) vec4<f32> {
    let offset = params[0].x * vec2<f32>(cos(params[0].y), sin(params[0].y));
    let r = sample_input(in.uv + offset).r;
    let g = sample_input(in.uv).g;
    let b = sample_input(in.uv - offset).b;
    return vec4<f32>(r, g, b, 1.0);
}

// ── 暗角 ──
// params[0] = (内圈, 外圈, 强度, _)：UV 到中心的距离从内圈到外圈渐暗。
@fragment
fn vignette(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let d = distance(in.uv, vec2<f32>(0.5));
    let shade = 1.0 - smoothstep(params[0].x, params[0].y, d) * params[0].z;
    return vec4<f32>(color.rgb * shade, color.a);
}

// ── 帧差 ──
// 第一个 pass 把输入原样存进 t0（它的历史就是「上一帧的输入」），
// 第二个 pass 按两帧之差给画面上色：静止的地方变灰，动的地方保持彩色。
// params[0].x = 放大倍数。
@fragment
fn difference_store(in: PostVertex) -> @location(0) vec4<f32> {
    return sample_input(in.uv);
}

@fragment
fn difference(in: PostVertex) -> @location(0) vec4<f32> {
    let current = sample_input(in.uv);
    var amount = 0.0;
    if (history_valid()) {
        let previous = sample_history(in.uv);
        amount = clamp(luminance(abs(previous.rgb - current.rgb)) * params[0].x, 0.0, 3.0);
    }
    let grey = vec3<f32>(luminance(current.rgb));
    return vec4<f32>(max(mix(grey, current.rgb, amount), vec3<f32>(0.0)), current.a);
}

// ── 转场 ──
// 从主相机（输入）过渡到 user0（通常是一台离屏相机的画面）。
// params[0] = (进度 0..1, 阈值, 用不用贴图, _)，user1 = 过渡贴图（灰度）。
@fragment
fn transition(in: PostVertex) -> @location(0) vec4<f32> {
    let a = sample_input(in.uv);
    let b = textureSampleLevel(user0, linear_sampler, in.uv, 0.0);
    let progress = clamp(params[0].x, 0.0, 1.0);
    if (params[0].z < 0.5) {
        return mix(a, b, progress);
    }
    let threshold = max(params[0].y, 1e-3);
    let pattern = textureSampleLevel(user1, linear_sampler, in.uv, 0.0).r;
    let r = progress * (1.0 + threshold * 2.0) - threshold;
    let t = clamp((r - pattern) / threshold, 0.0, 1.0);
    return mix(a, b, t);
}

// ── 调试视图 ──
// params[0].x：0 = 原样，1 = 深度，2 = 法线，3 = 运动向量，4 = SSAO，
// 5 = 遮罩，6 = 基础色，7 = 粗糙度 / 金属度，8 = 接触阴影。
@fragment
fn debug_view(in: PostVertex) -> @location(0) vec4<f32> {
    let mode = i32(params[0].x);
    var color = sample_input(in.uv).rgb;
    switch mode {
        case 1: {
            let d = linear_depth(in.uv) / max(params[0].y, 1e-3);
            color = vec3<f32>(1.0 - clamp(d, 0.0, 1.0));
        }
        case 2: { color = world_normal(in.uv) * 0.5 + 0.5; }
        case 3: {
            let v = velocity_uv(in.uv) * frame.resolution.xy * 0.05;
            color = vec3<f32>(v * 0.5 + 0.5, 0.5);
        }
        case 4: { color = vec3<f32>(ao_at(in.uv).r); }
        case 5: { color = mask_at(in.uv).rgb; }
        case 6: { color = material_at(in.uv).rgb; }
        case 7: { color = vec3<f32>(material_at(in.uv).a, metallic_at(in.uv), 0.0); }
        case 8: { color = vec3<f32>(ao_at(in.uv).g); }
        default: {}
    }
    // LDR 阶段：显示出来的就是这个值（链上是线性的，交换链会编码成 sRGB）。
    return vec4<f32>(srgb_to_linear(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0);
}
