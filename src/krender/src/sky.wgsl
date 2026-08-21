// 天空背景。用一个覆盖全屏的三角形，按逆投影求出每个像素的观察方向。

struct SkyGlobals {
    // 视图投影的逆矩阵，用于从裁剪空间反推世界方向
    inverse_view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    // x = 预滤波环境图的 mip 数（0 表示没有 HDR），其余保留
    ibl_params: vec4<f32>,
    environment: Environment,
};

@group(0) @binding(0) var<uniform> sky_globals: SkyGlobals;
// 和主 pass 共用同一张预滤波环境图。背景取第 0 级（最清晰的那级）。
@group(0) @binding(1) var sky_environment: texture_2d<f32>;
@group(0) @binding(2) var sky_environment_sampler: sampler;

struct SkyOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn sky_vs(@builtin(vertex_index) index: u32) -> SkyOutput {
    // 一个比屏幕更大的三角形，省掉第二个三角形与对角线上的重复着色。
    let ndc = vec2<f32>(
        f32((index << 1u) & 2u) * 2.0 - 1.0,
        f32(index & 2u) * 2.0 - 1.0,
    );

    var out: SkyOutput;
    // 深度取 1，配合 LessEqual 让天空只填充未被物体覆盖的像素。
    out.clip_position = vec4<f32>(ndc, 1.0, 1.0);
    out.ndc = ndc;
    return out;
}

@fragment
fn sky_fs(in: SkyOutput) -> @location(0) vec4<f32> {
    // 反投影到世界空间：取近平面上的一点，减去相机位置即为观察方向。
    let world = sky_globals.inverse_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(world.xyz / world.w - sky_globals.camera_position.xyz);

    // 有 HDR 就画 HDR，否则画程序化天空。
    //
    // 不接这一步的话，物体的反射来自 HDR、天上却是另一幅渐变天空，
    // 两者不一致时很容易看出来——尤其是水面和金属。
    var color: vec3<f32>;
    if (sky_globals.ibl_params.x > 0.5) {
        let uv = equirect_uv(direction);
        // 背景要最清晰的那级。用 `textureSampleLevel` 显式取第 0 级：
        // 让硬件自己挑的话，屏幕边缘的高变化率会挑到模糊的 mip，
        // 天空会糊掉一圈。
        color = textureSampleLevel(sky_environment, sky_environment_sampler, uv, 0.0).rgb
            * sky_globals.environment.sun_color.a;
    } else {
        color = ibl_sky(sky_globals.environment, direction);
    }
    // 同样输出线性 HDR，色调映射统一在后处理链里做。
    return vec4<f32>(color, 1.0);
}
