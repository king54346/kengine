// 接触阴影（屏幕空间阴影）。
//
// 阴影贴图有两个躲不开的毛病：分辨率有限，小物件（手指、螺丝、草叶）
// 投的影子不到一个纹素；以及为了防自遮挡加的偏移，会让影子**离开**
// 物体的底部，东西看起来是飘着的。
//
// 这一趟补的就是这两处：从每个像素出发，沿着「朝光源」的方向在深度
// 缓冲里走一小段（默认十几厘米）。途中哪一步落到了别的表面**后面**，
// 这个像素就在影子里。距离短，所以只管贴着地面的那一圈接触阴影，
// 大范围的影子仍然归阴影贴图。
//
// 结果写进遮蔽图的**绿通道**（红通道是 SSAO），主 pass 只把它乘在
// 阴影投射者（0 号光源）的可见度上——和阴影贴图同一个位置，
// 所以它和 SSAO 不同，是削**直射光**的。

struct ContactParams {
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    // xyz = 指向光源的方向（方向光）或光源位置（其他），w = 0 方向光 / 1 有位置
    light: vec4<f32>,
    // x = 走多远（世界单位），y = 厚度，z = 步数，w = 强度
    settings: vec4<f32>,
    // xy = 纹素尺寸，zw = 视口像素尺寸
    texel: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: ContactParams;
@group(0) @binding(1) var depth_texture: texture_depth_2d;

struct FullscreenOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn fullscreen_vs(@builtin(vertex_index) index: u32) -> FullscreenOutput {
    let ndc = vec2<f32>(
        f32((index << 1u) & 2u) * 2.0 - 1.0,
        f32(index & 2u) * 2.0 - 1.0,
    );
    var out: FullscreenOutput;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

fn load_depth(uv: vec2<f32>) -> f32 {
    let size = vec2<i32>(params.texel.zw);
    let pixel = clamp(vec2<i32>(uv * params.texel.zw), vec2<i32>(0), size - vec2<i32>(1));
    return textureLoad(depth_texture, pixel, 0);
}

fn world_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world = params.inverse_view_proj * ndc;
    return world.xyz / world.w;
}

// 交错梯度噪声（Jimenez 2014）。每个像素的起步偏一点点，
// 把「步长」造成的条带换成高频噪点——后者 TAA 抹得掉，前者抹不掉。
fn interleaved_gradient_noise(pixel: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(pixel, vec2<f32>(0.06711056, 0.00583715))));
}

@fragment
fn fs_main(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let depth = load_depth(in.uv);
    // 天空：没有表面，谈不上影子。
    if (depth >= 1.0) {
        return vec4<f32>(1.0);
    }

    let world = world_from_depth(in.uv, depth);
    var to_light = params.light.xyz;
    if (params.light.w > 0.5) {
        to_light = params.light.xyz - world;
    }
    let direction = normalize(to_light);

    let steps = max(params.settings.z, 1.0);
    let step_length = params.settings.x / steps;
    let thickness = params.settings.y;
    let noise = interleaved_gradient_noise(in.clip_position.xy);

    var occlusion = 0.0;
    for (var i = 0.0; i < steps; i = i + 1.0) {
        let travelled = (i + noise) * step_length;
        let sample_world = world + direction * travelled;
        let clip = params.view_proj * vec4<f32>(sample_world, 1.0);
        if (clip.w <= 0.0) {
            break;
        }
        let ndc = clip.xyz / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
            break;
        }

        // 在视空间距离（裁剪空间 w）上比，而不是在非线性的深度值上：
        // 「厚度」是个世界尺度的量，拿深度缓冲的值去比的话，远处的厚度
        // 会被压成零、近处的被放大成一堵墙。
        let scene_world = world_from_depth(uv, load_depth(uv));
        let scene_w = (params.view_proj * vec4<f32>(scene_world, 1.0)).w;
        let delta = clip.w - scene_w;
        if (delta > 0.002 * clip.w && delta < thickness) {
            // 离屏幕边缘越近越不可信（边外的遮挡者根本看不见），淡掉。
            let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
            occlusion = clamp(edge * 20.0, 0.0, 1.0);
            break;
        }
    }

    let visibility = 1.0 - occlusion * clamp(params.settings.w, 0.0, 1.0);
    return vec4<f32>(1.0, visibility, 1.0, 1.0);
}
