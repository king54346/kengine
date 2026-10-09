// 运动模糊 + 体积光。

// ── 运动模糊 ──
// params[0] = (强度, 采样数, _, _)
//
// 沿本像素的运动向量来回各走半程取样平均。强度 1 等于「快门开一整帧」；
// 电影通常是半帧（180° 快门），给 0.5。
@fragment
fn motion_blur(in: PostVertex) -> @location(0) vec4<f32> {
    let velocity = velocity_uv(in.uv) * params[0].x;
    let count = clamp(i32(params[0].y), 2, 64);
    // 抖一下起点：少量采样的条带换成噪点，TAA 或者肉眼都更容易接受。
    let noise = interleaved_gradient_noise(in.position.xy + frame.time.z) - 0.5;
    var total = vec4<f32>(0.0);
    for (var i = 0; i < count; i = i + 1) {
        let t = (f32(i) + noise) / f32(count - 1) - 0.5;
        total += sample_input(in.uv - velocity * t);
    }
    return total / f32(count);
}

// ── 体积光（god rays）──
// params[0] = (密度, 最大密度, 步数, 距离衰减指数)
// params[1] = (光源锚点 xyz, 锚点的作用范围)
// params[2] = (混合色 rgb, 模糊（0/1）)
//
// 从相机沿视线走到表面，每一步问阴影图「这里被灯照到了吗」，照到的就
// 往这条视线上累积一点。被柱子挡住的那些段没有贡献——于是光一条一条的。
//
// 累积方式和 three.js 的 GodraysNode 一样：每个样本加
// `照到 × 视线长度 × 密度 / 100 × (1 − 到锚点的距离 / 范围)^衰减指数`，
// 除以样本数，最后取 `1 − e^(−累积)` 并夹到最大密度——得到的是一个 [0, 最大密度]
// 的**雾浓度**，合成时往混合色上插值。离光源（锚点）越远越淡：
// 真实的体积光在光源附近最浓。

@fragment
fn godrays_march(in: PostVertex) -> @location(0) vec4<f32> {
    let depth = load_depth(in.uv);
    let camera = frame.camera_position.xyz;
    let end = world_position_from_depth(in.uv, min(depth, 0.99999));
    let ray = end - camera;
    let ray_length = length(ray);
    let steps = clamp(i32(params[0].z), 4, 256);
    // 步数按像素抖一点（和 three.js 一样），把条带换成噪点。
    let noise = interleaved_gradient_noise(in.position.xy);
    let samples = f32(steps) + round((f32(steps) / 8.0 + 2.0) * noise);

    let anchor = params[1].xyz;
    let range = max(params[1].w, 1e-3);
    var illumination = 0.0;
    for (var i = 0.0; i < samples; i = i + 1.0) {
        let position = camera + ray * (i / samples);
        let lit = sun_visibility(position);
        let falloff = pow(max(1.0 - distance(position, anchor) / range, 0.0), params[0].w);
        illumination += lit * ray_length * params[0].x / 100.0 * falloff;
    }
    // 除以样本数：累积的是「这条视线上被照到的比例 × 长度」，不随步数变。
    illumination /= samples;
    let fog = clamp(1.0 - exp(-illumination), 0.0, params[0].y);
    return vec4<f32>(vec3<f32>(fog), 1.0);
}

// 深度感知的模糊：跨过深度断层的样本权重衰减，柱子的边不会被光晕糊掉。
@fragment
fn godrays_blur(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(t0));
    let center_depth = linear_depth(in.uv);
    var total = 0.0;
    var weight_sum = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let offset_uv = in.uv + vec2<f32>(f32(x), f32(y)) * texel;
            let s = textureSampleLevel(t0, linear_sampler, offset_uv, 0.0).r;
            let d = linear_depth(offset_uv);
            let w = exp(-abs(d - center_depth) / max(center_depth * 0.05, 0.01)) * exp(-f32(x * x + y * y) * 0.2);
            total += s * w;
            weight_sum += w;
        }
    }
    return vec4<f32>(vec3<f32>(total / max(weight_sum, 1e-4)), 1.0);
}

@fragment
fn godrays_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    var amount = textureSampleLevel(t1, linear_sampler, in.uv, 0.0).r;
    if (params[2].w < 0.5) {
        amount = textureSampleLevel(t0, linear_sampler, in.uv, 0.0).r;
    }
    return vec4<f32>(mix(color.rgb, params[2].rgb, amount), color.a);
}
