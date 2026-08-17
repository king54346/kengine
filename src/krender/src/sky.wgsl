// 天空背景。用一个覆盖全屏的三角形，按逆投影求出每个像素的观察方向。

struct SkyGlobals {
    // 视图投影的逆矩阵，用于从裁剪空间反推世界方向
    inverse_view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    environment: Environment,
};

@group(0) @binding(0) var<uniform> sky_globals: SkyGlobals;

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

    // 同样输出线性 HDR，色调映射统一在后处理链里做。
    let color = ibl_sky(sky_globals.environment, direction);
    return vec4<f32>(color, 1.0);
}
