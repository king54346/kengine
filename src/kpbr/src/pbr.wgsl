// kpbr —— Cook-Torrance BRDF 的 WGSL 实现。
//
// 这段代码由渲染器拼接到标准着色器前面。
// 每个函数在 kpbr 的 Rust 侧都有一份等价实现，两者必须保持一致——
// Rust 侧那份用来在 CPU 上断言 BRDF 的数学性质（能量守恒、互易性等）。

const PBR_PI: f32 = 3.14159265359;
// 电介质的垂直入射反射率，约 4%，是业界通用近似值。
const PBR_DIELECTRIC_F0: f32 = 0.04;

// 法线分布函数：GGX / Trowbridge-Reitz。
// 描述微表面法线朝向半程向量的比例，决定高光的形状与拖尾。
fn pbr_distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    // 感知粗糙度平方后才是 GGX 的 alpha，这样滑块线性变化时观感才均匀。
    let a = roughness * roughness;
    let a2 = a * a;
    let n_dot_h2 = max(n_dot_h, 0.0) * max(n_dot_h, 0.0);

    let denominator = n_dot_h2 * (a2 - 1.0) + 1.0;
    return a2 / max(PBR_PI * denominator * denominator, 1e-7);
}

// 几何遮蔽项的单侧分量（Schlick-GGX）。
fn pbr_geometry_schlick_ggx(n_dot_x: f32, k: f32) -> f32 {
    let x = max(n_dot_x, 0.0);
    return x / max(x * (1.0 - k) + k, 1e-7);
}

// 几何遮蔽：微表面互相遮挡与阴影，用 Smith 方法把两侧相乘。
fn pbr_geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    // 直接光照用 (r+1)^2/8；IBL 应当改用 a/2，两者不可混用。
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return pbr_geometry_schlick_ggx(n_dot_v, k) * pbr_geometry_schlick_ggx(n_dot_l, k);
}

// 菲涅尔：掠射角下反射率趋近于 1，这是 PBR 观感的关键。
fn pbr_fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    let f = pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
    return f0 + (vec3<f32>(1.0) - f0) * f;
}

// 金属没有漫反射，其 F0 直接取基础色；电介质则统一用 4%。
fn pbr_f0(albedo: vec3<f32>, metallic: f32) -> vec3<f32> {
    return mix(vec3<f32>(PBR_DIELECTRIC_F0), albedo, clamp(metallic, 0.0, 1.0));
}

// 一盏直接光源的出射亮度。
//
// n/v/l 分别是法线、指向相机、指向光源的方向，均需已归一化。
fn pbr_direct_lighting(
    n: vec3<f32>,
    v: vec3<f32>,
    l: vec3<f32>,
    albedo: vec3<f32>,
    metallic: f32,
    roughness: f32,
    radiance: vec3<f32>,
) -> vec3<f32> {
    let h = normalize(v + l);
    let n_dot_v = max(dot(n, v), 0.0);
    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_h = max(dot(n, h), 0.0);
    let h_dot_v = max(dot(h, v), 0.0);

    // 背光面直接返回零，省去后续计算。
    if (n_dot_l <= 0.0) {
        return vec3<f32>(0.0);
    }

    let f0 = pbr_f0(albedo, metallic);
    let d = pbr_distribution_ggx(n_dot_h, roughness);
    let g = pbr_geometry_smith(n_dot_v, n_dot_l, roughness);
    let f = pbr_fresnel_schlick(h_dot_v, f0);

    let specular = (d * g * f) / max(4.0 * n_dot_v * n_dot_l, 1e-7);

    // 能量守恒：被镜面反射掉的能量不能再参与漫反射；金属则完全没有漫反射。
    let k_diffuse = (vec3<f32>(1.0) - f) * (1.0 - clamp(metallic, 0.0, 1.0));
    let diffuse = k_diffuse * albedo / PBR_PI;

    return (diffuse + specular) * radiance * n_dot_l;
}

// 极简环境光：用常量代替 IBL，保证背光面不会全黑。
// 真正的基于图像的光照留待后续。
fn pbr_ambient(albedo: vec3<f32>, occlusion: f32, ambient: vec3<f32>) -> vec3<f32> {
    return albedo * ambient * occlusion;
}
