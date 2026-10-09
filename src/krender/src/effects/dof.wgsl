// 景深。两种：
//
// - **简单版**（three.js `dof_basic`）：整张图做一次盒式模糊，再按
//   「离对焦面多远」在清晰和模糊之间插值。便宜，前景糊的时候边缘会
//   把背景「吸」进来——模糊版本不知道前后关系。
// - **散景版**（three.js `dof`）：先算每个像素的弥散圆（CoC），在半分辨率
//   上按 CoC 做圆盘采集（gather），亮点会被摊成一个个光斑。

// ── 简单版 ──
// params[0] = (对焦距离, 最小距离, 最大距离, _)，params[1] = (核半径, 间隔, _, _)

@fragment
fn box_blur(in: PostVertex) -> @location(0) vec4<f32> {
    let size = clamp(i32(params[1].x), 1, 4);
    let spread = params[1].y;
    let texel = frame.resolution.zw * spread;
    var total = vec4<f32>(0.0);
    var count = 0.0;
    for (var y = -size; y <= size; y = y + 1) {
        for (var x = -size; x <= size; x = x + 1) {
            total += sample_input(in.uv + vec2<f32>(f32(x), f32(y)) * texel);
            count += 1.0;
        }
    }
    return total / count;
}

@fragment
fn basic_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let sharp = sample_input(in.uv);
    let blurred = textureSampleLevel(t0, linear_sampler, in.uv, 0.0);
    let distance_from_focus = abs(linear_depth(in.uv) - params[0].x);
    let amount = smoothstep(params[0].y, params[0].z, distance_from_focus);
    return mix(sharp, blurred, amount);
}

// ── 散景版 ──
// params[0] = (对焦距离, 焦外过渡范围（three.js 的 focalLength）, 散景尺寸（半分辨率像素）, _)

// 带符号的 CoC：负 = 前景，正 = 背景，绝对值到 1 为止。
// 和 three.js 的 DepthOfFieldNode 一样用 smoothstep 过渡。
fn circle_of_confusion(uv: vec2<f32>) -> f32 {
    let distance_from_focus = linear_depth(uv) - params[0].x;
    return sign(distance_from_focus) * smoothstep(0.0, max(params[0].y, 1e-3), abs(distance_from_focus));
}

// 半分辨率：颜色 + CoC。四个样本取 CoC 绝对值最大的那个——
// 降采样时把小的前景物体的 CoC 平均掉，它的糊边就没了。
@fragment
fn bokeh_prepare(in: PostVertex) -> @location(0) vec4<f32> {
    let t = frame.resolution.zw;
    var color = vec3<f32>(0.0);
    var coc = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        let offset = vec2<f32>(f32(i & 1) - 0.5, f32(i >> 1u) - 0.5) * t;
        let sample_uv = in.uv + offset;
        color += sample_input(sample_uv).rgb;
        let c = circle_of_confusion(sample_uv);
        if (abs(c) > abs(coc)) {
            coc = c;
        }
    }
    return vec4<f32>(color * 0.25, coc);
}

// 圆盘采集：黄金角螺线上 48 个点。每个样本只在「它自己的弥散圆盖得到
// 这个像素」时才算数（scatter-as-gather），于是清晰的前景不会被身后
// 模糊的背景盖住，模糊的前景却能摊到清晰的背景上。
@fragment
fn bokeh_gather(in: PostVertex) -> @location(0) vec4<f32> {
    let center = textureSampleLevel(t0, nearest_sampler, in.uv, 0.0);
    let max_radius = params[0].z;
    let texel = 1.0 / vec2<f32>(textureDimensions(t0));
    var total = vec3<f32>(0.0);
    var weight_sum = 0.0;
    let golden_angle = 2.39996323;
    let count = 48;
    for (var i = 0; i < count; i = i + 1) {
        let r = sqrt((f32(i) + 0.5) / f32(count)) * max_radius;
        let theta = f32(i) * golden_angle;
        let offset = vec2<f32>(cos(theta), sin(theta)) * r;
        let s = textureSampleLevel(t0, linear_sampler, in.uv + offset * texel, 0.0);
        // 背景样本不能比中心更糊地盖过来：取两者 CoC 的较小值（前景除外）。
        var sample_coc = abs(s.a);
        if (s.a > 0.0) {
            sample_coc = min(sample_coc, abs(center.a) + 0.05);
        }
        let reach = sample_coc * max_radius;
        let w = clamp(reach - r + 1.0, 0.0, 1.0);
        total += s.rgb * w;
        weight_sum += w;
    }
    if (weight_sum < 1e-3) {
        return vec4<f32>(center.rgb, center.a);
    }
    return vec4<f32>(total / weight_sum, center.a);
}

// 3×3 帐篷：把采集的离散点抹平。
@fragment
fn bokeh_tent(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(t1));
    var total = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let w = (2.0 - abs(f32(x))) * (2.0 - abs(f32(y)));
            total += textureSampleLevel(t1, linear_sampler, in.uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0) * w;
        }
    }
    return total / 16.0;
}

@fragment
fn bokeh_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let sharp = sample_input(in.uv);
    let blurred = textureSampleLevel(t2, linear_sampler, in.uv, 0.0);
    // 全分辨率的 CoC 决定混多少——半分辨率的那份边缘是块状的。
    let coc = abs(circle_of_confusion(in.uv));
    let amount = smoothstep(0.05, 0.5, max(coc, abs(blurred.a) * step(blurred.a, 0.0)));
    return vec4<f32>(mix(sharp.rgb, blurred.rgb, amount), sharp.a);
}
