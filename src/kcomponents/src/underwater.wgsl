// 水下后处理（kcomponents::underwater）。
//
//   param_settings() = (启用 0/1, 雾浓度, 扭曲强度, 相机在水面下多深)
//   param_plane()    = 相机所在处的水面（法线 xyz, d）：dot(n, p) + d < 0 的点在水下
//   param_color()    = (水色 rgb, 光线从上方透下来的强度)
//   param_absorb()   = (每米吸收 rgb, 光柱强度)
//   param_sun()      = (指向太阳的方向 xyz, 太阳亮度)

fn uw_hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn uw_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(uw_hash(i), uw_hash(i + vec2<f32>(1.0, 0.0)), u.x), mix(uw_hash(i + vec2<f32>(0.0, 1.0)), uw_hash(i + vec2<f32>(1.0, 1.0)), u.x), u.y);
}

// 水面把阳光聚成一束束的：水面上哪儿亮。两层噪声相乘再锐化，亮处稀疏。
fn uw_shaft_pattern(q: vec2<f32>, time: f32) -> f32 {
    let a = uw_noise(q * 0.32 + vec2<f32>(time * 0.11, time * 0.07));
    let b = uw_noise(q * 0.71 - vec2<f32>(time * 0.05, -time * 0.13));
    return pow(clamp(a * b * 2.4, 0.0, 1.0), 2.0);
}

@fragment
fn underwater(in: PostVertex) -> @location(0) vec4<f32> {
    let settings = param_settings();
    if (settings.x < 0.5) {
        return sample_input(in.uv);
    }
    // 逐像素判断：这个像素的视线起点（近平面上那一点）在水面上还是水下。
    // 相机贴着水面时画面被水线一分为二——上半是空气、下半是水。
    let plane = param_plane();
    let near = world_position_from_depth(in.uv, 0.0);
    let time_wave = sin(near.x * 2.3 + frame.time.x * 1.7) * 0.015 + sin(near.z * 3.1 - frame.time.x * 2.1) * 0.01;
    let side = dot(plane.xyz, near) + plane.w + time_wave;
    if (side > 0.0) {
        // 水线正上方一窄条：水膜挂在镜头上，暗一点。
        let film = 1.0 - smoothstep(0.0, 0.012, side);
        return vec4<f32>(sample_input(in.uv).rgb * (1.0 - film * 0.35), 1.0);
    }
    let time = frame.time.x;
    // 水的折射让画面轻轻晃动。
    let wobble = vec2<f32>(
        sin(in.uv.y * 24.0 + time * 1.7) + sin(in.uv.y * 41.0 - time * 2.3) * 0.5,
        cos(in.uv.x * 19.0 + time * 1.3) + cos(in.uv.x * 33.0 + time * 2.9) * 0.5,
    ) * settings.z * 0.0015;
    let uv = clamp(in.uv + wobble, vec2<f32>(0.0), vec2<f32>(1.0));
    let color = sample_input(uv).rgb;
    let camera = frame.camera_position.xyz;
    let level = camera.y + settings.w;

    // 看出去多远：水下视线按距离吸收、散射。天（深度为空）当作很远。
    var view_distance = 400.0;
    if (!is_sky(uv)) {
        view_distance = length(world_position(uv) - camera);
    }
    // 水面是半透明的、不进深度缓冲：朝上看时深度里是几十公里外的天穹。
    // 视线最多走到水面那么远（相机深度 / 视线的竖直分量）。
    let ray = normalize(world_position_from_depth(uv, 0.5) - camera);
    if (ray.y > 0.01) {
        view_distance = min(view_distance, settings.w / ray.y);
    }
    let absorb = param_absorb().rgb;
    let transmittance = exp(-absorb * view_distance);

    // ── 雾的颜色：朝上看是透亮的水色，平视是深蓝，朝下更暗；相机越深整体越暗 ──
    let water = param_color().rgb;
    let depth_light = exp(-settings.w * 0.06);
    let upward = clamp(ray.y * 0.5 + 0.5, 0.0, 1.0);
    // 远处偏深蓝（不是水色本身的青绿）：红绿先被吸收掉，剩下的是蓝。
    let deep = vec3<f32>(0.002, 0.012, 0.04) + water * vec3<f32>(0.02, 0.05, 0.1);
    let fog_color = mix(deep, water * (0.6 + param_color().w), pow(upward, 4.0)) * depth_light;
    let fog = 1.0 - exp(-view_distance * settings.y);
    var result = mix(color * transmittance, fog_color, fog);

    // ── 光柱：沿视线步进，每一点顺着阳光方向投到水面上，看那里是不是亮的 ──
    let sun = param_sun();
    let strength = param_absorb().w;
    if (strength > 0.0 && sun.y > 0.05) {
        let steps = 24;
        let reach = min(view_distance, 70.0);
        let stride = reach / f32(steps);
        let jitter = interleaved_gradient_noise(in.position.xy);
        var shafts = 0.0;
        for (var i = 0; i < steps; i = i + 1) {
            let t = (f32(i) + jitter) * stride;
            let p = camera + ray * t;
            let d = level - p.y;
            if (d > 0.0) {
                let q = p.xz + sun.xz / max(sun.y, 0.25) * d;
                // 越深、越远越弱：光柱在水里一路被吸收、散开。
                shafts += uw_shaft_pattern(q, time) * exp(-d * 0.07) * exp(-t * 0.035);
            }
        }
        // 迎着太阳看光柱最亮（前向散射）。
        let toward_sun = pow(max(dot(ray, sun.xyz), 0.0), 3.0) * 1.5 + 0.35;
        result += (water * vec3<f32>(1.0, 1.3, 1.5) + vec3<f32>(0.01, 0.03, 0.05)) * sun.w * shafts * stride * 0.12 * strength * toward_sun * depth_light;
    }

    // 四角暗一点。
    let vignette = 1.0 - smoothstep(0.35, 0.9, distance(in.uv, vec2<f32>(0.5)));
    return vec4<f32>(result * mix(0.7, 1.0, vignette), 1.0);
}
