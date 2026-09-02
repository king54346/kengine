// kpbr —— 基于图像的光照（IBL）的 WGSL 实现。
//
// 由渲染器拼接到标准着色器前面。天空采样与球谐求值在 Rust 侧都有等价实现，
// 两者必须保持一致——环境光的球谐系数正是 CPU 侧用同一个天空函数积分出来的。

// 环境参数。与 Rust 侧的 GpuEnvironment 逐字段对应，改动需同步两边。
struct Environment {
    // 漫反射球谐系数，xyz 有效，w 为填充
    sh: array<vec4<f32>, 9>,
    // rgb = 天顶色
    zenith: vec4<f32>,
    // rgb = 地平线色
    horizon: vec4<f32>,
    // rgb = 地面色
    ground: vec4<f32>,
    // xyz = 指向太阳的方向，w = 太阳角半径余弦阈值
    sun_direction: vec4<f32>,
    // rgb = 太阳颜色，a = 环境光整体强度
    sun_color: vec4<f32>,
    // rgb = 雾色，a = 是否启用（0/1）
    fog_color: vec4<f32>,
    // x = 起雾距离，y = 全雾距离
    fog_params: vec4<f32>,
};

const IBL_PI: f32 = 3.14159265359;

// 线性雾的浓度。和 CPU 侧的 `Fog::density_at` 必须算出同样的结果。
fn fog_density(env: Environment, distance: f32) -> f32 {
    if (env.fog_color.a < 0.5) {
        return 0.0;
    }
    let span = env.fog_params.y - env.fog_params.x;
    // 区间退化时返回 0（完全清澈），不能除以零——NaN 会把整个像素染黑。
    if (span <= 1e-6) {
        return 0.0;
    }
    return clamp((distance - env.fog_params.x) / span, 0.0, 1.0);
}

// 把雾混进已经算好的颜色里。
fn apply_fog(env: Environment, color: vec3<f32>, distance: f32) -> vec3<f32> {
    return mix(color, env.fog_color.rgb, fog_density(env, distance));
}

// 不含太阳的天空。球谐系数就是对这个函数积分得到的，
// 太阳由一盏方向光单独表示，这里计入会导致重复曝光。
fn ibl_sky_base(env: Environment, direction: vec3<f32>) -> vec3<f32> {
    let up = direction.y;
    if (up >= 0.0) {
        return mix(env.horizon.rgb, env.zenith.rgb, sqrt(up));
    }
    return mix(env.horizon.rgb, env.ground.rgb, sqrt(-up));
}

// 含太阳的天空，用于绘制背景与低粗糙度的镜面反射。
fn ibl_sky(env: Environment, direction: vec3<f32>) -> vec3<f32> {
    let base = ibl_sky_base(env, direction);

    let cos_angle = dot(direction, env.sun_direction.xyz);
    let threshold = env.sun_direction.w;
    if (cos_angle > threshold) {
        let t = clamp((cos_angle - threshold) / max(1.0 - threshold, 1e-6), 0.0, 1.0);
        return base + env.sun_color.rgb * t * t;
    }
    return base;
}

// 球谐求值：9 个系数一次多项式展开，比采样辐照度贴图便宜得多。
// 系数在 CPU 侧已经乘过余弦卷积之外的部分，这里补上各阶的卷积因子。
// 球谐 → 辐照度。**系数由调用方给**，而不是从 `Environment` 里取。
//
// 拆出这一层是为了光照探针：每个探针有自己的一组系数，存在一块
// 存储缓冲里，按物体所属的探针层号去取。照着 `Environment` 写死的话，
// 一个场景就只能有一组漫反射环境光。
fn ibl_irradiance_from_sh(sh: array<vec4<f32>, 9>, n: vec3<f32>, intensity: f32) -> vec3<f32> {
    let c0 = 0.282095;
    let c1 = 0.488603;
    let c2 = 1.092548;
    let c3 = 0.315392;
    let c4 = 0.546274;

    // 余弦卷积系数：L0 为 π，L1 为 2π/3，L2 为 π/4。
    let a0 = IBL_PI;
    let a1 = 2.0 * IBL_PI / 3.0;
    let a2 = IBL_PI / 4.0;

    var result = sh[0].rgb * (c0 * a0);
    result += sh[1].rgb * (c1 * n.y * a1);
    result += sh[2].rgb * (c1 * n.z * a1);
    result += sh[3].rgb * (c1 * n.x * a1);
    result += sh[4].rgb * (c2 * n.x * n.y * a2);
    result += sh[5].rgb * (c2 * n.y * n.z * a2);
    result += sh[6].rgb * (c3 * (3.0 * n.z * n.z - 1.0) * a2);
    result += sh[7].rgb * (c2 * n.x * n.z * a2);
    result += sh[8].rgb * (c4 * (n.x * n.x - n.y * n.y) * a2);

    // 低阶球谐在高对比环境下可能出现轻微负值。
    return max(result, vec3<f32>(0.0)) * intensity;
}

// 镜面环境反射。
//
// 没有预滤波的 mip 链，改用近似：粗糙度低时取解析天空（锐利反射），
// 粗糙度高时退化到球谐辐照度（完全模糊），中间线性插值。
// `brdf` 是从 BRDF 查找表采样得到的 (scale, bias)。
fn ibl_specular(
    env: Environment,
    reflection: vec3<f32>,
    roughness: f32,
    f0: vec3<f32>,
    brdf: vec2<f32>,
) -> vec3<f32> {
    let sharp = ibl_sky_base(env, reflection);
    // 辐照度含 π 因子，除掉后才是平均辐射亮度，量纲上才能与 sharp 相加。
    let blurred = ibl_irradiance(env, reflection) / IBL_PI;

    let radiance = mix(sharp * env.sun_color.a, blurred, clamp(roughness, 0.0, 1.0));
    return radiance * (f0 * brdf.x + vec3<f32>(brdf.y));
}

// 方向转等距柱状投影的 UV。与 CPU 侧 `HdrImage::sample_direction` 一致——
// 两边不一致的话反射会整体偏转一个角度，而且不报错。
fn equirect_uv(direction: vec3<f32>) -> vec2<f32> {
    let d = normalize(direction);
    let u = (atan2(d.z, d.x) + IBL_PI) / (2.0 * IBL_PI);
    let v = acos(clamp(d.y, -1.0, 1.0)) / IBL_PI;
    return vec2<f32>(u, v);
}

// 从预滤波的 mip 链取镜面反射。
//
// mip 级由粗糙度线性选出。**必须用 `textureSampleLevel` 而不是
// `textureSample`**：后者按屏幕导数自己挑 mip，那算的是纹理在屏幕上的
// 缩放，和粗糙度毫无关系——粗糙的表面会得到清晰的镜像。
// 视差校正。和 CPU 侧的 `ReflectionProbe::correct` 必须算出同样的结果。
//
// slab 法求射线从盒内射出的距离：每个轴算出撞哪面墙的 t，取最小的那个。
// 取最大的话射线会「穿过」最近的墙，反射落在盒子外面。
fn parallax_correct(
    position: vec3<f32>,
    reflection: vec3<f32>,
    bounds_min: vec3<f32>,
    bounds_max: vec3<f32>,
    capture_position: vec3<f32>,
) -> vec3<f32> {
    // 往正方向走撞 max 面，反之撞 min 面。
    //
    // 某个轴的方向为零时这里会得到 ±inf，而 inf 参与 min 会被
    // 自动忽略（另外两轴的有限值更小）——正好是想要的行为，
    // 所以不用像 CPU 那边一样显式跳过。但三个轴同时为零时
    // 三个 t 都是 inf，下面的 `hit` 会变成 NaN，所以还要兜一次底。
    let inverse = 1.0 / reflection;
    let to_max = (bounds_max - position) * inverse;
    let to_min = (bounds_min - position) * inverse;
    // 每个轴取正的那个 t（另一个是负的，指向背后那面墙）。
    let furthest = max(to_max, to_min);
    let distance = min(min(furthest.x, furthest.y), furthest.z);

    if (!(distance > 0.0) || distance > 1e18) {
        // 距离非正、或者是 inf/NaN：射线打不到盒子，或者输入退化。
        // 退回未校正的方向——总比返回一个乱数好，NaN 采样出来是黑洞，
        // 而且会顺着 Bloom 扩散到整个画面。
        return reflection;
    }

    let hit = position + reflection * distance;
    let corrected = hit - capture_position;
    let length_squared = dot(corrected, corrected);
    if (length_squared < 1e-12) {
        return reflection;
    }
    return corrected * inverseSqrt(length_squared);
}

fn ibl_specular_prefiltered(
    prefiltered: texture_2d_array<f32>,
    prefiltered_sampler: sampler,
    // 纹理数组的层号。第 0 层是全局环境，1 起是各个反射探针。
    layer: f32,
    mip_count: f32,
    reflection: vec3<f32>,
    roughness: f32,
    f0: vec3<f32>,
    brdf: vec2<f32>,
    intensity: f32,
) -> vec3<f32> {
    let uv = equirect_uv(reflection);
    let level = clamp(roughness, 0.0, 1.0) * max(mip_count - 1.0, 0.0);
    let radiance = textureSampleLevel(
        prefiltered,
        prefiltered_sampler,
        uv,
        i32(layer),
        level,
    ).rgb;
    return radiance * intensity * (f0 * brdf.x + vec3<f32>(brdf.y));
}

// 漫反射环境贡献。金属没有漫反射，故按金属度衰减。

// 全局环境的辐照度。
fn ibl_irradiance(env: Environment, n: vec3<f32>) -> vec3<f32> {
    return ibl_irradiance_from_sh(env.sh, n, env.sun_color.a);
}

fn ibl_diffuse(
    env: Environment,
    n: vec3<f32>,
    albedo: vec3<f32>,
    metallic: f32,
    occlusion: f32,
) -> vec3<f32> {
    return ibl_diffuse_from_sh(env.sh, n, env.sun_color.a, albedo, metallic, occlusion);
}

// 用一组给定的球谐系数算漫反射环境光。光照探针走这条。
fn ibl_diffuse_from_sh(
    sh: array<vec4<f32>, 9>,
    n: vec3<f32>,
    intensity: f32,
    albedo: vec3<f32>,
    metallic: f32,
    occlusion: f32,
) -> vec3<f32> {
    let irradiance = ibl_irradiance_from_sh(sh, n, intensity);
    let k_diffuse = 1.0 - clamp(metallic, 0.0, 1.0);
    // 辐照度已含 π，Lambert BRDF 的 1/π 正好抵消。
    return albedo * irradiance * k_diffuse * occlusion / IBL_PI;
}
