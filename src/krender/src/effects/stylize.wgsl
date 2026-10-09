// 风格化：描边、像素化、复古（PS1 / CRT）。

// ── 描边 ──
// params[0].rgb = 看得见的边的颜色，params[1].rgb = 被挡住的边的颜色
// params[2] = (粗细（像素）, 强度, 辉光, 脉冲周期（秒，0 = 不闪）)
// params[3].x = 用遮罩的哪个通道
//
// 边 = 遮罩里「自己不在、邻居在」的像素。邻居里有看得见的（1）就是
// 看得见的边，只有被挡住的（0.5）就是藏在后面的边。

fn outline_mask(uv: vec2<f32>) -> f32 {
    return mask_at(uv)[clamp(i32(params[3].x), 0, 3)];
}

@fragment
fn outline_edges(in: PostVertex) -> @location(0) vec4<f32> {
    let thickness = max(params[2].x, 1.0);
    let texel = frame.resolution.zw * thickness;
    let center = outline_mask(in.uv);
    var visible = 0.0;
    var hidden = 0.0;
    for (var i = 0; i < 8; i = i + 1) {
        let angle = f32(i) * PI * 0.25;
        let m = outline_mask(in.uv + vec2<f32>(cos(angle), sin(angle)) * texel);
        visible = max(visible, step(0.75, m));
        hidden = max(hidden, step(0.25, m) * (1.0 - step(0.75, m)));
    }
    // 自己就在物体里的像素不算边（描边在轮廓外侧）。
    let outside = 1.0 - step(0.25, center);
    let visible_edge = visible * outside;
    let hidden_edge = hidden * (1.0 - visible) * outside;
    return vec4<f32>(visible_edge, hidden_edge, 0.0, 1.0);
}

// 半分辨率的模糊：辉光用。
@fragment
fn outline_glow(in: PostVertex) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(t0)) * 2.0;
    var total = vec4<f32>(0.0);
    var count = 0.0;
    for (var y = -3; y <= 3; y = y + 1) {
        for (var x = -3; x <= 3; x = x + 1) {
            let w = exp(-f32(x * x + y * y) / 6.0);
            total += textureSampleLevel(t0, linear_sampler, in.uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0) * w;
            count += w;
        }
    }
    return total / count;
}

@fragment
fn outline_composite(in: PostVertex) -> @location(0) vec4<f32> {
    let color = sample_input(in.uv);
    let edge = textureSampleLevel(t0, linear_sampler, in.uv, 0.0);
    let glow = textureSampleLevel(t1, linear_sampler, in.uv, 0.0);
    var pulse = 1.0;
    if (params[2].w > 0.0) {
        pulse = 0.5 + 0.5 * cos(frame.time.x * 2.0 * PI / params[2].w);
    }
    let visible_color = params[0].rgb;
    let hidden_color = params[1].rgb;
    let lines = (visible_color * edge.r + hidden_color * edge.g) * params[2].y;
    let halo = (visible_color * glow.r + hidden_color * glow.g) * params[2].z * 3.0;
    return vec4<f32>(color.rgb + (lines + halo) * pulse, color.a);
}

// ── 像素化 ──
// params[0] = (像素块大小, 法线边强度, 深度边强度, _)
//
// 和 three.js 的 PixelationPassNode 同一套：先按块取块中心的颜色，
// 再用块之间的深度差（外轮廓，压暗）和法线差（内部棱线，提亮）描边。

fn block_uv(block: vec2<f32>, size: f32) -> vec2<f32> {
    return (block + 0.5) * size * frame.resolution.zw;
}

@fragment
fn pixelate(in: PostVertex) -> @location(0) vec4<f32> {
    let size = max(floor(params[0].x), 1.0);
    let block = floor(in.position.xy / size);
    let uv = block_uv(block, size);
    var color = textureSampleLevel(input_texture, nearest_sampler, uv, 0.0);

    let depth = linear_depth(uv);
    let normal = world_normal(uv);
    var depth_diff = 0.0;
    var normal_indicator = 0.0;
    let offsets = array<vec2<f32>, 4>(
        vec2<f32>(0.0, -1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(-1.0, 0.0),
        vec2<f32>(1.0, 0.0),
    );
    for (var i = 0; i < 4; i = i + 1) {
        let neighbour_uv = block_uv(block + offsets[i], size);
        let neighbour_depth = linear_depth(neighbour_uv);
        // 用深度相对差：离得远的地方同样的像素跨度对应更大的深度差。
        let d = (neighbour_depth - depth) / max(depth, 1e-3);
        depth_diff += clamp(d, 0.0, 1.0);

        let neighbour_normal = world_normal(neighbour_uv);
        let normal_diff = dot(normal - neighbour_normal, vec3<f32>(1.0, 1.0, 1.0));
        let normal_side = clamp(smoothstep(-0.01, 0.01, normal_diff), 0.0, 1.0);
        let depth_side = clamp(sign(d * 0.25 + 0.0025), 0.0, 1.0);
        normal_indicator += (1.0 - dot(normal, neighbour_normal)) * depth_side * normal_side;
    }
    let depth_edge = floor(smoothstep(0.01, 0.02, depth_diff) * 2.0) / 2.0;
    let normal_edge = step(0.1, normal_indicator);

    if (depth_edge > 0.0) {
        color = vec4<f32>(color.rgb * (1.0 - params[0].z * depth_edge), color.a);
    } else {
        color = vec4<f32>(color.rgb * (1.0 + params[0].y * normal_edge), color.a);
    }
    return color;
}

// ── 复古 ──
// params[0] = (像素块大小, 每通道色阶数, 抖动强度, 扫描线强度)
// params[1] = (扫描线密度（条 / 屏高）, 暗角, 桶形畸变, 色彩渗漏)
//
// 顺序和 three.js 的 retro 例子一样：桶形畸变 → 降分辨率 → 色彩渗漏 →
// Bayer 抖动 + 色阶量化 → 扫描线 → 暗角。都在显示空间里做——
// 色阶量化在线性空间里做的话，暗部只剩一两档。

fn bayer4(pixel: vec2<i32>) -> f32 {
    let matrix = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    let index = (pixel.y & 3) * 4 + (pixel.x & 3);
    return matrix[index] / 16.0 - 0.5;
}

@fragment
fn retro(in: PostVertex) -> @location(0) vec4<f32> {
    // 桶形畸变：以中心为原点按半径的平方往外推。
    var uv = in.uv;
    let curvature = params[1].z;
    let centered = uv * 2.0 - 1.0;
    uv = (centered * (1.0 + curvature * dot(centered, centered)) / (1.0 + curvature)) * 0.5 + 0.5;
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let size = max(floor(params[0].x), 1.0);
    let pixel = floor(uv * frame.resolution.xy / size);
    let block_center = (pixel + 0.5) * size * frame.resolution.zw;
    var color = linear_to_srgb(textureSampleLevel(input_texture, nearest_sampler, block_center, 0.0).rgb);

    // 色彩渗漏：老电视的色度带宽比亮度窄，颜色会往右拖。
    let bleed = params[1].w;
    if (bleed > 0.0) {
        let left = linear_to_srgb(textureSampleLevel(input_texture, nearest_sampler, block_center - vec2<f32>(size * 2.0 * frame.resolution.z, 0.0), 0.0).rgb);
        color = mix(color, vec3<f32>(color.r * 0.5 + left.r * 0.5, color.g, color.b * 0.5 + left.b * 0.5), bleed);
    }

    let levels = max(params[0].y, 2.0);
    let dither = bayer4(vec2<i32>(pixel)) * params[0].z / (levels - 1.0);
    color = floor(clamp(color + dither, vec3<f32>(0.0), vec3<f32>(1.0)) * (levels - 1.0) + 0.5) / (levels - 1.0);

    let scanline = 0.5 + 0.5 * sin(in.uv.y * params[1].x * PI * 2.0);
    color *= 1.0 - params[0].w * (1.0 - scanline);

    let vignette = 1.0 - params[1].y * dot(centered, centered) * 0.5;
    color *= clamp(vignette, 0.0, 1.0);
    return vec4<f32>(srgb_to_linear(color), 1.0);
}
