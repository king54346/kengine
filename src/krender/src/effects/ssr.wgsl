// 屏幕空间反射（SSR）。
//
// params[0] = (最远距离, 厚度, 步数, 模糊强度)
// params[1] = (随机化（0 = 镜面方向，1 = 按粗糙度在 GGX 瓣里抖）, 对比分割线（<0 关）, 反射强度, _)
//
// 在视空间里沿反射方向步进，每一步投回屏幕、和深度缓冲比：走到某个
// 表面**后面**（但没超过厚度）就算打中，那个像素的颜色就是反射。
//
// 屏幕外、被挡住的东西反射不出来——这是 SSR 的本性，不是 bug。
// 所以打中的置信度要在屏幕边缘、射线走得很远、射线朝着相机时淡出，
// 淡出的部分由场景本来的反射（环境图 / 探针）兜底。

fn ssr_view_position(uv: vec2<f32>) -> vec3<f32> {
    return view_position(uv);
}

fn ssr_project(view: vec3<f32>) -> vec3<f32> {
    let clip = frame.projection * vec4<f32>(view, 1.0);
    let ndc = clip.xyz / clip.w;
    return vec3<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5, ndc.z);
}

// 在切线空间里按 GGX 分布采一个半程向量（Walter 2007）。
fn sample_ggx(xi: vec2<f32>, roughness: f32) -> vec3<f32> {
    let a = roughness * roughness;
    let phi = 2.0 * PI * xi.x;
    let cos_theta = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
    let sin_theta = sqrt(1.0 - cos_theta * cos_theta);
    return vec3<f32>(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta);
}

@fragment
fn ssr_trace(in: PostVertex) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let depth = load_depth(uv);
    if (depth >= 1.0) {
        return vec4<f32>(0.0);
    }
    let material = material_at(uv);
    let roughness = material.a;
    // 太糙的面反射不出清晰的东西，SSR 只会添噪点。
    if (roughness > 0.8) {
        return vec4<f32>(0.0);
    }

    let position = view_position_from_depth(uv, depth);
    let normal = view_normal(uv);
    let view_dir = normalize(position);
    var reflected = reflect(view_dir, normal);

    if (params[1].x > 0.0 && roughness > 0.02) {
        // 按粗糙度在 GGX 瓣里抖一个方向，每帧不同——时间性降噪再把它们平均掉。
        let seed = in.position.xy + vec2<f32>(frame.time.z * 17.0, frame.time.z * 31.0);
        let xi = vec2<f32>(hash12(seed), hash12(seed + 19.19));
        let up = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(normal.z) < 0.999);
        let tangent = normalize(cross(up, normal));
        let bitangent = cross(normal, tangent);
        let h = sample_ggx(xi, mix(0.0, roughness, params[1].x));
        let half_vector = normalize(tangent * h.x + bitangent * h.y + normal * h.z);
        reflected = reflect(view_dir, half_vector);
    }
    if (dot(reflected, normal) <= 0.0) {
        return vec4<f32>(0.0);
    }

    let max_distance = params[0].x;
    let thickness = params[0].y;
    let steps = clamp(i32(params[0].z), 8, 256);
    let step_length = max_distance / f32(steps);
    let jitter = interleaved_gradient_noise(in.position.xy + frame.time.z);

    var hit_uv = vec2<f32>(-1.0);
    var travelled = 0.0;
    var previous_t = 0.0;
    for (var i = 0; i < steps; i = i + 1) {
        let t = (f32(i) + jitter) * step_length;
        let sample_position = position + reflected * t;
        // 射线跑到相机后面就停：投影会翻过来。
        if (sample_position.z > -frame.camera.x) {
            break;
        }
        let screen = ssr_project(sample_position);
        if (any(screen.xy < vec2<f32>(0.0)) || any(screen.xy > vec2<f32>(1.0))) {
            break;
        }
        let scene_z = view_position(screen.xy).z;
        let delta = scene_z - sample_position.z;
        if (delta > 0.0 && delta < thickness) {
            // 二分细化：在上一步和这一步之间找交点。
            var low = previous_t;
            var high = t;
            for (var j = 0; j < 5; j = j + 1) {
                let middle = 0.5 * (low + high);
                let p = position + reflected * middle;
                let s = ssr_project(p);
                if (view_position(s.xy).z - p.z > 0.0) {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            hit_uv = ssr_project(position + reflected * high).xy;
            travelled = high;
            break;
        }
        previous_t = t;
    }
    if (hit_uv.x < 0.0) {
        return vec4<f32>(0.0);
    }

    let edge = min(min(hit_uv.x, 1.0 - hit_uv.x), min(hit_uv.y, 1.0 - hit_uv.y));
    let edge_fade = clamp(edge * 8.0, 0.0, 1.0);
    let distance_fade = 1.0 - clamp(travelled / max_distance, 0.0, 1.0);
    // 朝着相机的反射（reflected.z > 0）命中的多半是物体背面，不可信。
    let facing_fade = clamp(-reflected.z * 4.0 + 1.0, 0.0, 1.0);
    let confidence = edge_fade * distance_fade * facing_fade;
    return vec4<f32>(sample_input(hit_uv).rgb, confidence);
}

// 按粗糙度模糊：粗糙的面反射糊，镜面的清楚。
@fragment
fn ssr_blur(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(t0));
    let roughness = material_at(in.uv).a;
    let radius = roughness * params[0].w * 6.0;
    if (radius < 0.5) {
        return textureSampleLevel(t0, linear_sampler, in.uv, 0.0);
    }
    var total = vec4<f32>(0.0);
    var weight_sum = 0.0;
    let center_depth = linear_depth(in.uv);
    for (var i = 0; i < 16; i = i + 1) {
        let angle = f32(i) * 2.39996323;
        let r = sqrt((f32(i) + 0.5) / 16.0) * radius;
        let offset_uv = in.uv + vec2<f32>(cos(angle), sin(angle)) * r * texel;
        let w = exp(-abs(linear_depth(offset_uv) - center_depth) * 4.0);
        total += textureSampleLevel(t0, linear_sampler, offset_uv, 0.0) * w;
        weight_sum += w;
    }
    return total / max(weight_sum, 1e-4);
}

// 时间性累积：按运动向量取上一帧的结果（`history_texture` 是 t2 的上一帧），
// 用本帧邻域的范围夹一下再混。随机化的射线靠它变成干净的反射。
@fragment
fn ssr_temporal(in: PostVertex) -> @location(0) vec4<f32> {
    let current = textureSampleLevel(t1, linear_sampler, in.uv, 0.0);
    let previous_uv = in.uv - velocity_uv(in.uv);
    if (!history_valid() || any(previous_uv < vec2<f32>(0.0)) || any(previous_uv > vec2<f32>(1.0))) {
        return current;
    }
    let texel = 1.0 / vec2<f32>(textureDimensions(t1));
    var low = current;
    var high = current;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let s = textureSampleLevel(t1, linear_sampler, in.uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0);
            low = min(low, s);
            high = max(high, s);
        }
    }
    let history = clamp(sample_history(previous_uv), low, high);
    return mix(history, current, 0.1);
}

@fragment
fn ssr_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    // 用哪一张：有时间性那一步就是 t2，否则 t1。
    var reflection = textureSampleLevel(t1, linear_sampler, in.uv, 0.0);
    if (params[1].w > 0.5) {
        reflection = textureSampleLevel(t2, linear_sampler, in.uv, 0.0);
    }
    // 分屏对比：分割线左边是原始（没模糊没累积）的那张。
    let split = params[1].y;
    if (split >= 0.0 && in.uv.x < split) {
        reflection = textureSampleLevel(t0, linear_sampler, in.uv, 0.0);
    }
    if (split >= 0.0 && abs(in.uv.x - split) < frame.resolution.z) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }

    let material = material_at(in.uv);
    let metallic = metallic_at(in.uv);
    let roughness = material.a;
    // 反射率：金属是基础色，非金属是 4% 的菲涅尔基底，再按粗糙度压一压。
    let f0 = mix(vec3<f32>(0.04), material.rgb, metallic);
    let n = world_normal(in.uv);
    let v = normalize(frame.camera_position.xyz - world_position(in.uv));
    let cos_theta = clamp(dot(n, v), 0.0, 1.0);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
    let gloss = (1.0 - roughness) * (1.0 - roughness);
    let weight = fresnel * gloss * reflection.a * params[1].z;
    return vec4<f32>(color.rgb + reflection.rgb * weight, color.a);
}
