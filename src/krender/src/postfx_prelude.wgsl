// ── kengine 后处理前缀 ──
//
// 每个 `PostEffect` 的 WGSL 前面都会拼上这一段。效果只需要写片元入口：
//
//     @fragment
//     fn main(in: PostVertex) -> @location(0) vec4<f32> {
//         return sample_input(in.uv) * param_strength().x;
//     }
//
// 顶点着色器（`post_vs`，一个盖满屏幕的三角形）由引擎提供。
// `param_<名字>()` 是按效果声明的参数生成的，见 `PostEffect::param`。

struct PostFrame {
    // 带抖动的视图投影，和深度缓冲逐像素对得上——从深度重建位置用它。
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    // 不带抖动的本帧 / 上一帧视图投影。重投影用。
    clip_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    inverse_projection: mat4x4<f32>,
    // xyz = 相机位置
    camera_position: vec4<f32>,
    // xy = 输出的像素尺寸，zw = 倒数
    resolution: vec4<f32>,
    // x = 秒，y = 帧间隔，z = 帧号，w 保留
    time: vec4<f32>,
    // x = 近平面，y = 远平面，z = 1 透视 / 0 正交，w = 曝光
    camera: vec4<f32>,
    // xy = 本帧抖动（UV 单位），zw = 上一帧的
    jitter: vec4<f32>,
    // 0 号光源：xyz = 指向光源的方向（w = 0）或光源位置（w = 1）；w = -1 没有
    light: vec4<f32>,
    // rgb = 0 号光源的颜色 × 强度
    light_color: vec4<f32>,
    // 阴影图各层的矩阵：方向光是级联，点光是立方体六面，聚光只有第 0 层。
    light_view_proj: array<mat4x4<f32>, 6>,
    // x/y/z = 前三级的远距离，w = 实际级数
    cascade_splits: vec4<f32>,
    // x = 投射者类型（0 没有，1 方向光，2 点光，3 聚光），y = 深度偏移（方向光，
    // 归一化深度），z = 深度偏移（点光 / 聚光，世界单位），w 保留
    shadow_params: vec4<f32>,
};

struct EffectInfo {
    // x = 这个效果已经跑了几帧（重建目标后从 0 数起），
    // y = 历史纹理可不可用（0/1），zw 保留
    state: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: PostFrame;
@group(0) @binding(1) var<uniform> params: array<vec4<f32>, 16>;
// 链上的上一环：HDR 阶段是场景颜色（或上一个效果的结果），LDR 阶段是色调映射之后的。
@group(0) @binding(2) var input_texture: texture_2d<f32>;
// 主 pass 的深度（带 TAA 抖动时也带着抖动）。
@group(0) @binding(3) var depth_texture: texture_depth_2d;
// 预通道：xyz = 世界法线，w = 金属度。没开预通道时是全零。
@group(0) @binding(4) var normal_texture: texture_2d<f32>;
// 预通道：本帧 NDC − 上一帧 NDC。
@group(0) @binding(5) var velocity_texture: texture_2d<f32>;
// 后处理遮罩：四个通道各一位，1 = 看得见，0.5 = 被挡住。
@group(0) @binding(6) var mask_texture: texture_2d<f32>;
// 这个效果上一帧的输出（或声明的那张暂存图）。
@group(0) @binding(7) var history_texture: texture_2d<f32>;
// r = SSAO，g = 接触阴影。不可过滤，用 `textureLoad`。
@group(0) @binding(8) var ao_texture: texture_2d<f32>;
// 预通道：rgb = 基础色，a = 粗糙度。
@group(0) @binding(9) var material_texture: texture_2d<f32>;
// 未经任何效果的场景 HDR 颜色。LDR 阶段也能拿到。
@group(0) @binding(10) var scene_texture: texture_2d<f32>;
@group(0) @binding(11) var linear_sampler: sampler;
@group(0) @binding(12) var nearest_sampler: sampler;
@group(0) @binding(13) var repeat_sampler: sampler;
@group(0) @binding(14) var<uniform> effect: EffectInfo;
// 阴影投射者（0 号光源）的级联阴影图。体积光要沿视线逐点问「这儿被照到了吗」。
@group(0) @binding(15) var shadow_map: texture_depth_2d_array;
@group(0) @binding(16) var shadow_sampler: sampler_comparison;

// 效果自己的暂存图，由 `PostEffect::scratch` 声明。
// 正在写的那张绑的是占位图——一张纹理不能同时读和写。
@group(1) @binding(0) var t0: texture_2d<f32>;
@group(1) @binding(1) var t1: texture_2d<f32>;
@group(1) @binding(2) var t2: texture_2d<f32>;
@group(1) @binding(3) var t3: texture_2d<f32>;
// 用户贴图：`PostEffect::texture` 给的图，或者 `PostEffect::view` 指定的
// 离屏相机画面（线性 HDR，见 `CameraTarget::View`）。
//
// 离屏视图不单开绑定：一个着色阶段最多 16 张采样纹理（WebGPU 的下限），
// 前面的输入已经占了 14 张。
@group(1) @binding(4) var user0: texture_2d<f32>;
@group(1) @binding(5) var user1: texture_2d<f32>;

struct PostVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn post_vs(@builtin(vertex_index) index: u32) -> PostVertex {
    let ndc = vec2<f32>(
        f32((index << 1u) & 2u) * 2.0 - 1.0,
        f32(index & 2u) * 2.0 - 1.0,
    );
    var out: PostVertex;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

// ── 常用函数 ──

const PI: f32 = 3.14159265358979;

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn sample_input(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(input_texture, linear_sampler, uv, 0.0);
}

fn sample_scene(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(scene_texture, linear_sampler, uv, 0.0);
}

fn sample_history(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(history_texture, linear_sampler, uv, 0.0);
}

fn history_valid() -> bool {
    return effect.state.y > 0.5;
}

fn pixel_of(texture: texture_2d<f32>, uv: vec2<f32>) -> vec2<i32> {
    let size = vec2<i32>(textureDimensions(texture));
    return clamp(vec2<i32>(uv * vec2<f32>(size)), vec2<i32>(0), size - vec2<i32>(1));
}

// 深度缓冲的原始值，`[0, 1]`，1 是天空。
fn load_depth(uv: vec2<f32>) -> f32 {
    let size = vec2<i32>(textureDimensions(depth_texture));
    let pixel = clamp(vec2<i32>(uv * vec2<f32>(size)), vec2<i32>(0), size - vec2<i32>(1));
    return textureLoad(depth_texture, pixel, 0);
}

fn is_sky(uv: vec2<f32>) -> bool {
    return load_depth(uv) >= 1.0;
}

fn world_position_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world = frame.inverse_view_proj * ndc;
    return world.xyz / world.w;
}

fn world_position(uv: vec2<f32>) -> vec3<f32> {
    return world_position_from_depth(uv, load_depth(uv));
}

fn view_position_from_depth(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let view = frame.inverse_projection * ndc;
    return view.xyz / view.w;
}

fn view_position(uv: vec2<f32>) -> vec3<f32> {
    return view_position_from_depth(uv, load_depth(uv));
}

// 到相机的视空间距离（正数）。天空给远平面。
fn linear_depth(uv: vec2<f32>) -> f32 {
    let depth = load_depth(uv);
    if (depth >= 1.0) {
        return frame.camera.y;
    }
    return -view_position_from_depth(uv, depth).z;
}

// 世界点投到屏幕：xy = UV，z = 深度缓冲值，w = 视空间距离。
fn project_to_screen(world: vec3<f32>) -> vec4<f32> {
    let clip = frame.view_proj * vec4<f32>(world, 1.0);
    let ndc = clip.xyz / clip.w;
    return vec4<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5, ndc.z, clip.w);
}

fn world_normal(uv: vec2<f32>) -> vec3<f32> {
    let n = textureLoad(normal_texture, pixel_of(normal_texture, uv), 0).xyz;
    let length_squared = dot(n, n);
    if (length_squared < 1e-8) {
        return vec3<f32>(0.0);
    }
    return n * inverseSqrt(length_squared);
}

fn view_normal(uv: vec2<f32>) -> vec3<f32> {
    return normalize((frame.view * vec4<f32>(world_normal(uv), 0.0)).xyz + vec3<f32>(1e-6));
}

fn metallic_at(uv: vec2<f32>) -> f32 {
    return textureLoad(normal_texture, pixel_of(normal_texture, uv), 0).w;
}

fn material_at(uv: vec2<f32>) -> vec4<f32> {
    return textureLoad(material_texture, pixel_of(material_texture, uv), 0);
}

// 运动向量换成 UV 单位：当前 UV − 上一帧的 UV。
fn velocity_uv(uv: vec2<f32>) -> vec2<f32> {
    let v = textureLoad(velocity_texture, pixel_of(velocity_texture, uv), 0).xy;
    return v * vec2<f32>(0.5, -0.5);
}

fn mask_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(mask_texture, linear_sampler, uv, 0.0);
}

fn ao_at(uv: vec2<f32>) -> vec2<f32> {
    return textureLoad(ao_texture, pixel_of(ao_texture, uv), 0).rg;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let clamped = max(c, vec3<f32>(0.0));
    let low = clamped * 12.92;
    let high = 1.055 * pow(clamped, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, clamped <= vec3<f32>(0.0031308));
}

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// 交错梯度噪声（Jimenez 2014）：每像素一个 [0,1) 的数，帧号可以拿来滚动。
fn interleaved_gradient_noise(pixel: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(pixel, vec2<f32>(0.06711056, 0.00583715))));
}

fn rgb_to_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        0.25 * c.r + 0.5 * c.g + 0.25 * c.b,
        0.5 * c.r - 0.5 * c.b,
        -0.25 * c.r + 0.5 * c.g - 0.25 * c.b,
    );
}

// 世界里的一点有没有被 0 号光源照到（1 = 照到）。没有阴影时恒为 1。
//
// 单次比较采样，不做 PCF：体积光一条视线上要问几十次，每次再做 PCF
// 是白花力气——沿视线的累积本身就是一次大范围的平均。
fn sun_visibility(world: vec3<f32>) -> f32 {
    let kind = frame.shadow_params.x;
    if (kind < 0.5) {
        return 1.0;
    }
    var layer = 0;
    var position = world;
    var bias = frame.shadow_params.y;
    if (kind < 1.5) {
        let distance_to_camera = distance(world, frame.camera_position.xyz);
        let count = i32(frame.cascade_splits.w);
        layer = count - 1;
        for (var i = 0; i < 3; i = i + 1) {
            if (i < count && distance_to_camera < frame.cascade_splits[i]) {
                layer = i;
                break;
            }
        }
        layer = max(layer, 0);
    } else {
        // 透视的阴影图：偏移在世界空间里做，朝光源挪一点。
        position += normalize(frame.light.xyz - world) * frame.shadow_params.z;
        bias = 0.0;
        if (kind < 2.5) {
            let d = world - frame.light.xyz;
            let a = abs(d);
            if (a.x >= a.y && a.x >= a.z) {
                layer = select(1, 0, d.x > 0.0);
            } else if (a.y >= a.z) {
                layer = select(3, 2, d.y > 0.0);
            } else {
                layer = select(5, 4, d.z > 0.0);
            }
        }
    }
    let clip = frame.light_view_proj[layer] * vec4<f32>(position, 1.0);
    // 聚光：阴影图的视锥就是光锥。锥外（含光源背后）根本照不到，返回 0——
    // 以前一律当「照到」，聚光的体积光会糊满整个画面而不是一道光柱。
    let spot = kind > 2.5;
    if (clip.w <= 0.0) {
        return select(1.0, 0.0, spot);
    }
    let ndc = clip.xyz / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z > 1.0) {
        return select(1.0, 0.0, spot);
    }
    // 光锥是阴影图里内切的那个圆，边缘软一点。
    let cone = select(1.0, 1.0 - smoothstep(0.75, 1.0, length(uv - 0.5) * 2.0), spot);
    return cone * textureSampleCompareLevel(shadow_map, shadow_sampler, uv, layer, ndc.z - bias);
}

fn ycocg_to_rgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
}
