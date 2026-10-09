// 时间性抗锯齿（TAA）。
//
// 每帧投影挪一个亚像素（Halton 2,3，八帧一圈），于是同一个像素在不同帧
// 采到的是它覆盖范围里的不同位置。把历史帧按运动向量对齐之后和本帧
// 按 9:1 混合，几帧下来就等于在像素里采了很多点——静止画面上的效果
// 接近几十倍超采样。
//
// 三件事决定了它好不好：
//
// 1. **重投影用最近的深度**：运动向量取 3×3 邻域里离相机最近的那个像素的。
//    物体边缘上的像素一半是前景一半是背景，直接取本像素的速度会让前景
//    的边拖出一条背景的尾巴。
// 2. **历史要夹**：拿本帧 3×3 邻域的颜色范围（YCoCg 空间的方差包围盒）
//    去夹历史颜色。被遮挡物露出来、灯开关了，历史就是错的——
//    不夹的话会留下残影。
// 3. **按亮度加权混合**（Karis 2014）：HDR 里一个 100 的高光和旁边 0.1
//    的像素混 10%，结果还是 10，闪烁根本压不下去。先按 1/(1+亮度)
//    压一下再混、混完再还原，高光就安分了。

fn taa_weight(color: vec3<f32>) -> f32 {
    return 1.0 / (1.0 + luminance(color));
}

// Catmull-Rom 采样历史（Jimenez 的 5 次双线性优化版）。
// 双线性重投影每帧糊一点，几十帧累积下来整个画面会软掉；
// Catmull-Rom 是带负瓣的锐利滤波，抵消掉这份损失。
fn sample_history_catmull_rom(uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(history_texture));
    let position = uv * size;
    let center = floor(position - 0.5) + 0.5;
    let f = position - center;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = -0.5 * f3 + f2 - 0.5 * f;
    let w1 = 1.5 * f3 - 2.5 * f2 + 1.0;
    let w2 = -1.5 * f3 + 2.0 * f2 + 0.5 * f;
    let w3 = 0.5 * f3 - 0.5 * f2;
    let w12 = w1 + w2;
    let tc12 = (center + w2 / w12) / size;
    let tc0 = (center - 1.0) / size;
    let tc3 = (center + 2.0) / size;

    var result = vec3<f32>(0.0);
    result += sample_history(vec2<f32>(tc12.x, tc0.y)).rgb * (w12.x * w0.y);
    result += sample_history(vec2<f32>(tc0.x, tc12.y)).rgb * (w0.x * w12.y);
    result += sample_history(vec2<f32>(tc12.x, tc12.y)).rgb * (w12.x * w12.y);
    result += sample_history(vec2<f32>(tc3.x, tc12.y)).rgb * (w3.x * w12.y);
    result += sample_history(vec2<f32>(tc12.x, tc3.y)).rgb * (w12.x * w3.y);
    let weight = w12.x * w0.y + w0.x * w12.y + w12.x * w12.y + w3.x * w12.y + w12.x * w3.y;
    return max(result / weight, vec3<f32>(0.0));
}

// 把 `history` 沿着指向包围盒中心的方向拉回盒子里（比逐分量 clamp 少偏色）。
fn clip_to_box(history: vec3<f32>, low: vec3<f32>, high: vec3<f32>) -> vec3<f32> {
    let center = 0.5 * (high + low);
    let extent = 0.5 * (high - low) + vec3<f32>(1e-5);
    let offset = history - center;
    let units = abs(offset / extent);
    let largest = max(units.x, max(units.y, units.z));
    if (largest > 1.0) {
        return center + offset / largest;
    }
    return history;
}

@fragment
fn resolve(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = frame.resolution.zw;
    let current = sample_input(in.uv).rgb;

    // 3×3 邻域：颜色的一阶二阶矩（YCoCg），和离相机最近的那个像素。
    var mean = vec3<f32>(0.0);
    var squares = vec3<f32>(0.0);
    var closest_depth = 0.0;
    var closest_uv = in.uv;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset_uv = in.uv + vec2<f32>(f32(x), f32(y)) * texel;
            let sample_color = rgb_to_ycocg(textureSampleLevel(input_texture, nearest_sampler, offset_uv, 0.0).rgb);
            mean += sample_color;
            squares += sample_color * sample_color;
            // 深度缓冲是 1 在远处（标准 Z），「最近」就是最小的那个。
            let depth = 1.0 - load_depth(offset_uv);
            if (depth > closest_depth) {
                closest_depth = depth;
                closest_uv = offset_uv;
            }
        }
    }
    mean /= 9.0;
    let deviation = sqrt(max(squares / 9.0 - mean * mean, vec3<f32>(0.0)));
    // γ = 1：包围盒只比均值宽一个标准差。再宽的话被遮挡物露出时的残影明显。
    let low = mean - deviation * 1.0;
    let high = mean + deviation * 1.0;

    let previous_uv = in.uv - velocity_uv(closest_uv);
    let outside = any(previous_uv < vec2<f32>(0.0)) || any(previous_uv > vec2<f32>(1.0));
    if (!history_valid() || outside) {
        return vec4<f32>(current, 1.0);
    }

    var history = sample_history_catmull_rom(previous_uv);
    history = ycocg_to_rgb(clip_to_box(rgb_to_ycocg(history), low, high));

    // 动得越快越相信本帧：历史在高速运动里本来就不准，还会带出拖影。
    let speed = length(velocity_uv(closest_uv) * frame.resolution.xy);
    let blend = clamp(mix(0.1, 0.3, speed / 20.0), 0.1, 0.3);

    let w_current = taa_weight(current) * blend;
    let w_history = taa_weight(history) * (1.0 - blend);
    let result = (current * w_current + history * w_history) / max(w_current + w_history, 1e-5);
    return vec4<f32>(result, 1.0);
}
