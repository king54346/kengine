// 镜头：变形宽银幕光条、镜头光晕。

// ── 变形宽银幕（anamorphic）──
// params[0] = (阈值, 强度, 采样数, 步长（全分辨率像素）)，params[1].rgb = 色调
//
// 宽银幕镜头的柱面透镜只在水平方向压缩，于是亮点的眩光被拉成一条
// 横线。做法：提亮部 → 在四分之一分辨率上只沿水平方向做一次很宽的模糊。

@fragment
fn anamorphic_bright(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv).rgb;
    let l = luminance(color);
    let alpha = smoothstep(params[0].x, params[0].x + 0.2, l);
    return vec4<f32>(min(color * alpha, vec3<f32>(100.0)), 1.0);
}

@fragment
fn anamorphic_streak(in: PostVertex) -> @location(0) vec4<f32> {
    let count = clamp(i32(params[0].z), 2, 256);
    let half_count = f32(count) * 0.5;
    // 步长按全分辨率的像素算，和 three.js 一样：光条的长度不随这张图的
    // 分辨率变。
    let texel = frame.resolution.z * params[0].w;
    var total = vec3<f32>(0.0);
    for (var i = 0; i < count; i = i + 1) {
        let offset = f32(i) - half_count;
        // 越远越淡，二次方衰减：中间一小段很亮，两头拖得很长很淡。
        let softness = pow(1.0 - abs(offset) / half_count, 2.0);
        total += textureSampleLevel(t0, linear_sampler, vec2<f32>(in.uv.x + offset * texel, in.uv.y), 0.0).rgb * softness;
    }
    // three.js 的归一化：除以采样数的三分之一（二次方衰减的积分是 1/3）。
    return vec4<f32>(total / (f32(count) / 3.0), 1.0);
}

@fragment
fn anamorphic_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let streak = textureSampleLevel(t1, linear_sampler, in.uv, 0.0).rgb;
    return vec4<f32>(color.rgb + streak * params[1].rgb * params[0].y, color.a);
}

// ── 镜头光晕（伪光晕，John Chapman 2013）──
// params[0] = (阈值, 鬼影间距, 鬼影衰减, 鬼影个数)
// params[1] = (光环宽度, 光环强度, 色散, 总强度)
//
// 真实镜头里，亮光源在镜片之间反射，会沿「光源 → 画面中心」的连线
// 留下一串小光斑（鬼影），外加一圈光环。这里把亮部图**以画面中心为
// 原点翻转**，再沿那条线采样几次，就得到了那串鬼影。

@fragment
fn flare_bright(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv).rgb;
    let l = luminance(color);
    return vec4<f32>(min(color * smoothstep(params[0].x, params[0].x * 2.0 + 0.1, l), vec3<f32>(50.0)), 1.0);
}

fn chromatic_sample(uv: vec2<f32>, direction: vec2<f32>, distortion: f32) -> vec3<f32> {
    return vec3<f32>(
        textureSampleLevel(t0, linear_sampler, uv + direction * distortion, 0.0).r,
        textureSampleLevel(t0, linear_sampler, uv, 0.0).g,
        textureSampleLevel(t0, linear_sampler, uv - direction * distortion, 0.0).b,
    );
}

@fragment
fn flare_ghosts(in: PostVertex) -> @location(0) vec4<f32> {
    let flipped = vec2<f32>(1.0) - in.uv;
    let ghost_vector = (vec2<f32>(0.5) - flipped) * params[0].y;
    let direction = normalize(ghost_vector + vec2<f32>(1e-5));
    let distortion = params[1].z * 0.01;
    let count = clamp(i32(params[0].w), 1, 16);
    var result = vec3<f32>(0.0);
    for (var i = 0; i < count; i = i + 1) {
        let offset = fract(flipped + ghost_vector * f32(i));
        // 离中心越远越淡：只有靠近中心的鬼影亮，符合真实镜头。
        let weight = pow(max(1.0 - length(vec2<f32>(0.5) - offset) / 0.7071, 0.0), params[0].z);
        result += chromatic_sample(offset, direction, distortion) * weight;
    }
    // 光环：沿同一方向走一个固定长度，只在画面边缘一圈显出来。
    let halo_uv = fract(flipped + direction * params[1].x);
    let halo_weight = pow(max(1.0 - length(vec2<f32>(0.5) - halo_uv) / 0.7071, 0.0), 5.0);
    result += chromatic_sample(halo_uv, direction, distortion) * halo_weight * params[1].y;
    return vec4<f32>(result, 1.0);
}

@fragment
fn flare_blur(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(t1));
    var total = vec3<f32>(0.0);
    var weight_sum = 0.0;
    for (var y = -3; y <= 3; y = y + 1) {
        for (var x = -3; x <= 3; x = x + 1) {
            let w = exp(-f32(x * x + y * y) / 8.0);
            total += textureSampleLevel(t1, linear_sampler, in.uv + vec2<f32>(f32(x), f32(y)) * texel * 1.5, 0.0).rgb * w;
            weight_sum += w;
        }
    }
    return vec4<f32>(total / weight_sum, 1.0);
}

@fragment
fn flare_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let flare = textureSampleLevel(t2, linear_sampler, in.uv, 0.0).rgb;
    return vec4<f32>(color.rgb + flare * params[1].w, color.a);
}
