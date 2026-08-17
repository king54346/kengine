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
};

const IBL_PI: f32 = 3.14159265359;

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
fn ibl_irradiance(env: Environment, n: vec3<f32>) -> vec3<f32> {
    let c0 = 0.282095;
    let c1 = 0.488603;
    let c2 = 1.092548;
    let c3 = 0.315392;
    let c4 = 0.546274;

    // 余弦卷积系数：L0 为 π，L1 为 2π/3，L2 为 π/4。
    let a0 = IBL_PI;
    let a1 = 2.0 * IBL_PI / 3.0;
    let a2 = IBL_PI / 4.0;

    var result = env.sh[0].rgb * (c0 * a0);
    result += env.sh[1].rgb * (c1 * n.y * a1);
    result += env.sh[2].rgb * (c1 * n.z * a1);
    result += env.sh[3].rgb * (c1 * n.x * a1);
    result += env.sh[4].rgb * (c2 * n.x * n.y * a2);
    result += env.sh[5].rgb * (c2 * n.y * n.z * a2);
    result += env.sh[6].rgb * (c3 * (3.0 * n.z * n.z - 1.0) * a2);
    result += env.sh[7].rgb * (c2 * n.x * n.z * a2);
    result += env.sh[8].rgb * (c4 * (n.x * n.x - n.y * n.y) * a2);

    // 低阶球谐在高对比环境下可能出现轻微负值。
    return max(result, vec3<f32>(0.0)) * env.sun_color.a;
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

// 漫反射环境贡献。金属没有漫反射，故按金属度衰减。
fn ibl_diffuse(
    env: Environment,
    n: vec3<f32>,
    albedo: vec3<f32>,
    metallic: f32,
    occlusion: f32,
) -> vec3<f32> {
    let irradiance = ibl_irradiance(env, n);
    let k_diffuse = 1.0 - clamp(metallic, 0.0, 1.0);
    // 辐照度已含 π，Lambert BRDF 的 1/π 正好抵消。
    return albedo * irradiance * k_diffuse * occlusion / IBL_PI;
}
