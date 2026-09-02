// 屏幕空间环境光遮蔽（SSAO）。
//
// 读预通道产出的深度和世界法线，算出每个像素「被周围几何挡住多少」，
// 输出一张单通道的遮蔽图。主着色器把它乘进 `occlusion`，
// 于是只削弱**环境光**，直射光不受影响——这一点很要紧：
// 把 AO 直接乘在最终颜色上（很多引擎图省事的做法）会把太阳照亮的地方
// 也一起压暗，看着像脏了一层灰。
//
// # 半球采样
//
// 在以像素表面为原点、法线为轴的半球里撒一把点，把每个点投回屏幕，
// 比较「这个点的深度」和「深度图里那个位置的深度」。被挡住的点越多，
// 遮蔽越强。
//
// 半球而不是整球：整球的话平坦表面也会有一半的采样点落在表面下方，
// 到处都是 0.5 的灰。

struct SsaoParams {
    /// 从裁剪空间回到世界空间。深度反解位置要用。
    inverse_view_proj: mat4x4<f32>,
    /// 世界 → 裁剪。把采样点投回屏幕要用。
    view_proj: mat4x4<f32>,
    /// 相机位置，w 未使用。
    camera_position: vec4<f32>,
    /// x = 采样半径（世界单位），y = 强度，z = 采样数，w = 深度偏移
    settings: vec4<f32>,
    /// xy = 纹素尺寸，zw = 视口像素尺寸
    texel: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: SsaoParams;
@group(0) @binding(1) var depth_texture: texture_depth_2d;
@group(0) @binding(2) var normal_texture: texture_2d<f32>;

/// 全屏三角形。三个顶点盖住整个视口，比两个三角形少一条对角线上的
/// 重复着色，也不会在接缝处出现裂纹。
@vertex
fn fullscreen_vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = 1.0 - f32(index & 2u) * 2.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

/// 从深度图反解世界坐标。
fn world_position_at(coord: vec2<i32>, uv: vec2<f32>) -> vec3<f32> {
    let depth = textureLoad(depth_texture, coord, 0);
    // 裁剪空间：xy 从 [0,1] 的 UV 变回 [-1,1]，y 要翻（UV 向下、NDC 向上）。
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world = params.inverse_view_proj * clip;
    // 透视除法。w 为 0 只可能出现在退化的矩阵上，夹一下免得出 NaN——
    // NaN 会顺着乘法传遍整张 AO 图，表现为整个画面忽然全黑。
    return world.xyz / max(world.w, 1e-6);
}

/// 一个稳定的伪随机方向，按像素坐标取。
///
/// 每个像素转一个不同的角度，把有限的采样数摊成噪声而不是条纹——
/// 不转的话所有像素用同一组方向，平面上会出现规则的同心环。
fn rotation_at(coord: vec2<i32>) -> vec2<f32> {
    let seed = f32(coord.x) * 12.9898 + f32(coord.y) * 78.233;
    let angle = fract(sin(seed) * 43758.5453) * 6.2831853;
    return vec2<f32>(cos(angle), sin(angle));
}

/// 半球里的一组固定方向，切空间（法线是 +Z）。
///
/// 两条性质是必须的，少一条 AO 就不对：
///
/// - **z 恒为正**。有负的就意味着往表面**里面**采样，平坦的地方会凭空
///   出现一半的遮蔽，整个画面糊上一层灰。
/// - **长度不一，且短的居多**。全都是单位长的话采样点全落在半球壳上，
///   近处的接触阴影（墙角、物体和地面的交线）一点都采不到——
///   而那正是 AO 最该表现的地方。
///
/// 写死而不是传进来：16 个 `vec3` 是 256 字节的 uniform，而它们从头到尾
/// 不变。真正让相邻像素不一样的是上面那个旋转。
const KERNEL: array<vec3<f32>, 16> = array<vec3<f32>, 16>(
    vec3<f32>( 0.0353,  0.0264,  0.0612),
    vec3<f32>(-0.0713,  0.0468,  0.0854),
    vec3<f32>( 0.0917, -0.1024,  0.1338),
    vec3<f32>(-0.1420, -0.0836,  0.1877),
    vec3<f32>( 0.1934,  0.1650,  0.2263),
    vec3<f32>( 0.0428, -0.2506,  0.2745),
    vec3<f32>(-0.2731,  0.1187,  0.3208),
    vec3<f32>( 0.3016,  0.2444,  0.3624),
    vec3<f32>(-0.1259, -0.3702,  0.4033),
    vec3<f32>(-0.3846,  0.1830,  0.4491),
    vec3<f32>( 0.4127, -0.3055,  0.4914),
    vec3<f32>( 0.1673,  0.5052,  0.5326),
    vec3<f32>(-0.5330, -0.2216,  0.5748),
    vec3<f32>( 0.2609, -0.5644,  0.6160),
    vec3<f32>(-0.3162,  0.6031,  0.6580),
    vec3<f32>( 0.6449,  0.3547,  0.6991),
);

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) f32 {
    let coord = vec2<i32>(position.xy);
    let uv = position.xy * params.texel.xy;

    let depth = textureLoad(depth_texture, coord, 0);
    // 天空：深度是清除值，那里没有几何，遮蔽为 0（完全不遮）。
    // 不特判的话反解出来的位置在无穷远，采样全落空，结果是随机噪声。
    if (depth >= 1.0) {
        return 1.0;
    }

    let origin = world_position_at(coord, uv);
    let normal = normalize(textureLoad(normal_texture, coord, 0).xyz);

    // 用旋转向量和法线建一个正交基（Gram-Schmidt）。
    let rotation = rotation_at(coord);
    let random = vec3<f32>(rotation.x, rotation.y, 0.0);
    let tangent = normalize(random - normal * dot(random, normal));
    let bitangent = cross(normal, tangent);
    let tbn = mat3x3<f32>(tangent, bitangent, normal);

    let radius = params.settings.x;
    let bias = params.settings.w;
    let count = i32(params.settings.z);

    var occlusion = 0.0;
    for (var i = 0; i < 16; i = i + 1) {
        if (i >= count) {
            break;
        }
        // 采样点：半球里的方向转到表面的切空间，按半径缩放。
        let offset = tbn * KERNEL[i];
        let sample_world = origin + offset * radius;

        // 投回屏幕。
        let clip = params.view_proj * vec4<f32>(sample_world, 1.0);
        if (clip.w <= 0.0) {
            continue;
        }
        let ndc = clip.xyz / clip.w;
        let sample_uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        // 采样点跑出屏幕就跳过。硬当成「没遮挡」的话，画面边缘会亮一圈。
        if (sample_uv.x < 0.0 || sample_uv.x > 1.0 || sample_uv.y < 0.0 || sample_uv.y > 1.0) {
            continue;
        }

        let sample_coord = vec2<i32>(sample_uv * params.texel.zw);
        let scene = world_position_at(sample_coord, sample_uv);

        // 「深度图里那个位置」比采样点更靠近相机 → 采样点被挡住了。
        let to_camera = params.camera_position.xyz;
        let sample_distance = length(sample_world - to_camera);
        let scene_distance = length(scene - to_camera);

        if (scene_distance < sample_distance - bias) {
            // 范围检查：挡住它的东西如果离得很远（比如背景里的一堵墙），
            // 不该算作遮蔽。不做这一步的话，物体的轮廓外面会糊一圈黑边。
            let range = radius / max(abs(sample_distance - scene_distance), 1e-4);
            occlusion = occlusion + clamp(range, 0.0, 1.0);
        }
    }

    let strength = params.settings.y;
    // 输出的是「透光率」：1 = 完全不遮，0 = 全黑。
    // 主着色器直接乘进 `occlusion`，所以这个方向省掉一次取反。
    return clamp(1.0 - occlusion / f32(max(count, 1)) * strength, 0.0, 1.0);
}
