// 水下表面的材质钩子（kcomponents::caustics）：海床、岛的水下部分、码头的桩。
//
//   params[0] = (海平面高度, 焦散强度, 焦散尺度（米）, 湿润带宽度（米）)
//   params[1] = (每米吸收 rgb, _)
//
// 水面以下：直射光先按水深吸收（红光没得最快），再乘一层随时间流动的焦散——
// 水面起伏把阳光聚成一条条亮线。水线附近一圈「湿」的：颜色变深、变光滑。

fn caustic_hash(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(dot(p, vec2<f32>(127.1, 311.7)), dot(p, vec2<f32>(269.5, 183.3)));
    return fract(sin(q) * 43758.5453);
}

// 细胞噪声的「边」：到最近和次近特征点的距离差，边上是 0。特征点随时间绕圈。
fn caustic_cells(p: vec2<f32>, time: f32) -> f32 {
    let base = floor(p);
    let f = fract(p);
    var first = 8.0;
    var second = 8.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let cell = vec2<f32>(f32(x), f32(y));
            let h = caustic_hash(base + cell);
            let point = cell + 0.5 + 0.42 * sin(time * (0.6 + h * 0.5) + 6.2831 * h);
            let d = length(point - f);
            if (d < first) {
                second = first;
                first = d;
            } else if (d < second) {
                second = d;
            }
        }
    }
    return second - first;
}

fn caustic_pattern(p: vec2<f32>, time: f32) -> f32 {
    // 两层不同尺度相乘、往不同方向漂：一层的话格子感太重。
    let a = caustic_cells(p, time);
    let b = caustic_cells(p * 1.7 + vec2<f32>(time * 0.07, -time * 0.05), time * 1.3);
    // 亮线要软：硬边的细胞纹在浅水里看起来像龟裂的地砖。
    let ridge_a = pow(1.0 - smoothstep(0.0, 0.32, a), 2.0);
    let ridge_b = pow(1.0 - smoothstep(0.0, 0.3, b), 2.0);
    return ridge_a * 0.6 + ridge_b * 0.5 + ridge_a * ridge_b * 0.6;
}

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    let level = surface.params[0].x;
    let wet_band = surface.params[0].w;
    let above = surface.world_position.y - level;
    // 水线以上一小段是湿的：沙子颜色深、更光滑。
    let wet = 1.0 - smoothstep(0.0, wet_band, above);
    out.base_color = vec4<f32>(out.base_color.rgb * mix(1.0, 0.55, wet), out.base_color.a);
    out.roughness = mix(out.roughness, 0.25, wet * 0.8);
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    let lit = pbr_direct_lighting(
        (*surface).normal, (*surface).view_direction, input.light_direction,
        (*surface).base_color.rgb, (*surface).metallic, (*surface).roughness,
        input.radiance,
    );
    let level = (*surface).params[0].x;
    let depth = level - (*surface).world_position.y;
    if (depth <= 0.0) {
        return lit;
    }
    // 光从水面斜着照下来：按光线方向把采样点挪回它穿过水面的位置。
    let to_surface = (*surface).world_position.xz + input.light_direction.xz / max(input.light_direction.y, 0.2) * depth;
    let scale = max((*surface).params[0].z, 0.1);
    let caustic = caustic_pattern(to_surface / scale, (*surface).time * 0.8);
    // 越深焦散越糊（光线散开了）、越弱。
    let sharpness = exp(-depth * 0.06);
    let absorb = exp(-(*surface).params[1].xyz * depth);
    let fade_in = smoothstep(0.0, 0.6, depth);
    return lit * absorb * mix(1.0, 0.7 + caustic * (*surface).params[0].y * sharpness * 1.1, fade_in);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    let ambient = input.diffuse + input.specular + input.hemisphere;
    let depth = (*surface).params[0].x - (*surface).world_position.y;
    if (depth <= 0.0) {
        return ambient;
    }
    // 水下的环境光也被水吸收、染成水色。
    return ambient * exp(-(*surface).params[1].xyz * depth * 1.4);
}
