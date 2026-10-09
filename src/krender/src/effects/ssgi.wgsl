// 屏幕空间全局光照（SSGI）+ 环境光遮蔽。
//
// params[0] = (切片数, 每片步数, 半径（世界单位）, 厚度)
// params[1] = (GI 强度, AO 强度, 时间性（0/1）, 输出：0 合成 / 1 只看 GI / 2 只看 AO)
//
// 每个像素朝几个方向（切片）在屏幕上步进，看周围的表面：
//
// - 在法线半球里、离得够近的表面**挡住**了一部分天空 → AO；
// - 那些表面自己被照亮的颜色**反弹**到这里 → GI。
//
// 反弹只有一次，而且只看得到屏幕上有的东西：一面红墙在画面外，
// 地板上就不会有红色的反光。结果是半分辨率、带噪声的，后面两步
// （时间性累积 + 双边模糊）把它抹干净。

@fragment
fn ssgi_trace(in: PostVertex) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let depth = load_depth(uv);
    if (depth >= 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let position = view_position_from_depth(uv, depth);
    let normal = view_normal(uv);
    let slices = clamp(i32(params[0].x), 1, 8);
    let steps = clamp(i32(params[0].y), 1, 32);
    let radius = params[0].z;
    let thickness = params[0].w;

    // 世界半径换成屏幕上的像素长度：离得越远，同样的半径在屏幕上越短。
    let projected_radius = radius * frame.projection[1][1] * 0.5 / max(-position.z, 1e-3);
    let noise = interleaved_gradient_noise(in.position.xy + frame.time.z * 5.588238);
    let step_noise = hash12(in.position.xy + frame.time.z);

    // 每个方向（切片的两侧）只认**沿途最高的地平线**：它挡住了多少天空
    // 就是这个方向的遮蔽，它自己被照亮的颜色就是这个方向反弹来的光。
    // 按步平均的话，大部分空步会把结果稀释到几乎看不见。
    var gi = vec3<f32>(0.0);
    var occlusion = 0.0;
    for (var s = 0; s < slices; s = s + 1) {
        let angle = (f32(s) + noise) / f32(slices) * PI;
        let direction = vec2<f32>(cos(angle), sin(angle));
        for (var side = -1; side <= 1; side = side + 2) {
            var best_cosine = 0.05;
            var best_light = vec3<f32>(0.0);
            var best_weight = 0.0;
            for (var i = 0; i < steps; i = i + 1) {
                let t = (f32(i) + step_noise) / f32(steps);
                let offset = direction * f32(side) * projected_radius * t * t;
                let sample_uv = uv + offset * vec2<f32>(1.0, frame.resolution.x * frame.resolution.w);
                if (any(sample_uv < vec2<f32>(0.0)) || any(sample_uv > vec2<f32>(1.0))) {
                    break;
                }
                let sample_depth = load_depth(sample_uv);
                if (sample_depth >= 1.0) {
                    continue;
                }
                let sample_position = view_position_from_depth(sample_uv, sample_depth);
                let delta = sample_position - position;
                let distance_to_sample = length(delta);
                if (distance_to_sample < 1e-4 || distance_to_sample > radius) {
                    continue;
                }
                let to_sample = delta / distance_to_sample;
                let cosine = dot(normal, to_sample);
                // 厚度：样本如果比自己在屏幕上看起来的位置「深」太多，
                // 多半是远处的背景，不算挡住。
                let behind = clamp(1.0 - (-delta.z - thickness) / thickness, 0.0, 1.0);
                if (cosine > best_cosine && behind > 0.0) {
                    best_cosine = cosine;
                    let falloff = 1.0 - distance_to_sample / radius;
                    // 反弹：那个表面被照亮的颜色，按它朝向这里的程度加权。
                    let sample_normal = view_normal(sample_uv);
                    let emit = clamp(dot(sample_normal, -to_sample) * 0.5 + 0.5, 0.0, 1.0);
                    best_light = sample_input(sample_uv).rgb * emit;
                    best_weight = falloff * behind;
                }
            }
            if (best_weight > 0.0) {
                gi += best_light * best_cosine * best_weight;
                occlusion += best_cosine * best_weight;
            }
        }
    }
    let directions = f32(slices * 2);
    let ao = clamp(1.0 - occlusion / directions, 0.0, 1.0);
    // 换成辐照度：半球上的余弦加权平均乘 π，每个方向代表的是一整片
    // 剖面（两侧各一半），再乘 2。这个 2π 是按截图和 three.js 的参考图
    // 对着定的——不乘的话箱子朝着地板的那面收不到反弹光，是黑的。
    return vec4<f32>(gi / directions * 2.0 * PI, ao);
}

@fragment
fn ssgi_temporal(in: PostVertex) -> @location(0) vec4<f32> {
    let current = textureSampleLevel(t0, linear_sampler, in.uv, 0.0);
    if (params[1].z < 0.5) {
        return current;
    }
    let previous_uv = in.uv - velocity_uv(in.uv);
    if (!history_valid() || any(previous_uv < vec2<f32>(0.0)) || any(previous_uv > vec2<f32>(1.0))) {
        return current;
    }
    let texel = 1.0 / vec2<f32>(textureDimensions(t0));
    var mean = vec4<f32>(0.0);
    var squares = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let s = textureSampleLevel(t0, linear_sampler, in.uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0);
            mean += s;
            squares += s * s;
        }
    }
    mean /= 9.0;
    let deviation = sqrt(max(squares / 9.0 - mean * mean, vec4<f32>(0.0)));
    let history = clamp(sample_history(previous_uv), mean - deviation * 1.5, mean + deviation * 1.5);
    return mix(history, current, 0.1);
}

// 双边模糊：按深度和法线的差距衰减权重，不把光抹过物体的边。
@fragment
fn ssgi_denoise(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(t1));
    let center_depth = linear_depth(in.uv);
    let center_normal = world_normal(in.uv);
    var total = vec4<f32>(0.0);
    var weight_sum = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let offset_uv = in.uv + vec2<f32>(f32(x), f32(y)) * texel;
            let depth_weight = exp(-abs(linear_depth(offset_uv) - center_depth) / max(center_depth * 0.02, 0.01));
            let normal_weight = pow(max(dot(world_normal(offset_uv), center_normal), 0.0), 8.0);
            let w = depth_weight * normal_weight * exp(-f32(x * x + y * y) * 0.15);
            total += textureSampleLevel(t1, linear_sampler, offset_uv, 0.0) * w;
            weight_sum += w;
        }
    }
    return total / max(weight_sum, 1e-4);
}

@fragment
fn ssgi_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let result = textureSampleLevel(t2, linear_sampler, in.uv, 0.0);
    let albedo = material_at(in.uv).rgb;
    let ao = mix(1.0, result.a, clamp(params[1].y, 0.0, 1.0));
    let gi = result.rgb * albedo * params[1].x;
    let mode = i32(params[1].w);
    if (mode == 1) {
        return vec4<f32>(result.rgb * params[1].x, 1.0);
    }
    if (mode == 2) {
        return vec4<f32>(vec3<f32>(ao), 1.0);
    }
    return vec4<f32>(color.rgb * ao + gi, color.a);
}
