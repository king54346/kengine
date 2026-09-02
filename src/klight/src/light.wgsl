// klight —— 光源求值的 WGSL 实现。
//
// 由渲染器拼接到标准着色器前面。每个函数在 Rust 侧都有等价实现，
// 两者必须保持一致——Rust 那份用于在 CPU 上断言衰减曲线的性质。

// 与 Rust 侧的 GpuLight 逐字段对应，改动需同步两边。
struct Light {
    // xyz = 世界坐标（方向光未使用），w = 类型：0 方向光 / 1 点光源 / 2 聚光灯
    position: vec4<f32>,
    // xyz = 光线传播方向（方向光与聚光灯使用），w = 作用半径
    direction: vec4<f32>,
    // rgb = 颜色，a = 强度
    color: vec4<f32>,
    // 聚光灯：x = 内锥余弦，y = 外锥余弦
    // 半球光：xyz = 地面色
    // 矩形面光源：x = 半宽，y = 半高
    params: vec4<f32>,
    // x = 照亮哪些层的位掩码，y = cookie 层号（0 = 没有），其余保留
    extra: vec4<u32>,
    // xyz = 光源的世界右轴（已归一化），w = 聚光灯的 tan(外锥角)
    right: vec4<f32>,
};

const LIGHT_DIRECTIONAL: f32 = 0.0;
const LIGHT_POINT: f32 = 1.0;
const LIGHT_SPOT: f32 = 2.0;
const LIGHT_HEMISPHERE: f32 = 3.0;
const LIGHT_RECT: f32 = 4.0;

// 距离衰减：物理上的平方反比，再乘一个窗函数在 range 处平滑归零。
//
// 纯 1/d² 永远不会真正到 0，会导致远处光源仍需参与计算；
// 窗函数让光源有明确的作用范围，便于后续做光源剔除。
fn light_distance_attenuation(distance: f32, range: f32) -> f32 {
    if (range <= 0.0) {
        return 0.0;
    }
    // 分母加 1 避免距离趋零时除爆。
    let falloff = 1.0 / (1.0 + distance * distance);

    let ratio = clamp(distance / range, 0.0, 1.0);
    let ratio2 = ratio * ratio;
    let window = clamp(1.0 - ratio2 * ratio2, 0.0, 1.0);

    return falloff * window * window;
}

// 聚光灯的锥形衰减：内锥内为 1，内外锥之间平滑过渡，外锥外为 0。
fn light_spot_attenuation(cos_angle: f32, cos_inner: f32, cos_outer: f32) -> f32 {
    // 内外锥重合时不能除零，退化为硬边缘。
    let denominator = cos_inner - cos_outer;
    if (denominator <= 1e-5) {
        return select(0.0, 1.0, cos_angle >= cos_outer);
    }
    let t = clamp((cos_angle - cos_outer) / denominator, 0.0, 1.0);
    // 平方一下让边缘过渡更柔和。
    return t * t;
}

// 求某个光源在给定着色点上的入射方向与辐射亮度。
// 返回 xyz = 指向光源的单位向量，w = 该方向上的衰减系数。
fn light_sample_direction(light: Light, world_position: vec3<f32>) -> vec4<f32> {
    if (light.position.w == LIGHT_DIRECTIONAL) {
        // 方向光没有位置，也不衰减。
        return vec4<f32>(normalize(-light.direction.xyz), 1.0);
    }

    let to_light = light.position.xyz - world_position;
    let distance = length(to_light);
    if (distance <= 1e-5) {
        return vec4<f32>(0.0, 1.0, 0.0, 0.0);
    }
    let l = to_light / distance;

    var attenuation = light_distance_attenuation(distance, light.direction.w);

    if (light.position.w == LIGHT_RECT) {
        // 矩形面光源的方向取「指向代表点」，而不是指向中心：
        // 贴着板子的表面看到的是最近的那一块，不是中心。
        let closest = light_rect_closest(light, world_position);
        let to_closest = closest - world_position;
        let closest_distance = length(to_closest);
        if (closest_distance <= 1e-5) {
            return vec4<f32>(0.0, 1.0, 0.0, 0.0);
        }
        return vec4<f32>(
            to_closest / closest_distance,
            light_distance_attenuation(closest_distance, light.direction.w),
        );
    }

    if (light.position.w == LIGHT_SPOT) {
        // 着色点相对聚光灯轴线的夹角余弦。
        let cos_angle = dot(normalize(light.direction.xyz), -l);
        attenuation *= light_spot_attenuation(cos_angle, light.params.x, light.params.y);
    }

    return vec4<f32>(l, attenuation);
}

// 聚光灯的 cookie UV：把着色点投到光的成像平面上。
//
// 这就是一次以光源为视点的透视投影，只是不必真的建一个矩阵——
// 光轴、右轴、上轴三者构成一个正交基，把「从光指向着色点」的向量
// 拆到这个基上，再除以「沿光轴的距离 × tan(外锥角)」就得到 [-1,1]。
//
// 上轴由 `cross(方向, 右轴)` 得到而不是单独存一个：三者正交，
// 存两个就够，第三个是叉积。
//
// 返回 [0,1] 的 UV。锥外的点会落在 [0,1] 之外，但**不特判**——
// 锥形衰减那一步已经把锥外的强度压成 0 了，这里再判一次是白花的分支。
fn light_cookie_uv(light: Light, world_position: vec3<f32>) -> vec2<f32> {
    let forward = normalize(light.direction.xyz);
    let right = normalize(light.right.xyz);
    let up = cross(forward, right);

    let to_point = world_position - light.position.xyz;
    // 沿光轴的距离。太小的话除下来会爆，夹一个下限。
    let axial = max(dot(to_point, forward), 1e-4);
    // 外锥在这个距离上的半径。`tan` 在 CPU 侧算好了。
    let extent = max(axial * light.right.w, 1e-6);

    let u = dot(to_point, right) / extent;
    let v = dot(to_point, up) / extent;
    // [-1,1] → [0,1]。v 取反：贴图的 v 向下，而上轴向上。
    return vec2<f32>(u * 0.5 + 0.5, 0.5 - v * 0.5);
}

// ── 矩形面光源 ──
//
// 用的**不是** LTC（线性变换余弦）。LTC 要两张拟合出来的查找表，
// 而那些表是离线拟合的产物，没法在引擎里生成。这里走的是另一条路：
//
// - **漫反射**：矩形对着色点张成的立体角，有闭式解（Lambert 1760 的
//   多边形形状因子）。这一半是**精确的**，不是近似。
// - **高光**：代表点近似（MRP）——在矩形上找一个离「理想反射方向」
//   最近的点，当成一盏点光源。这一半是近似的：掠射角下高光的形状
//   会比真实的短一点。
//
// 取舍写在这儿：LTC 的高光更准，但它要的那两张表这个引擎给不出来，
// 而漫反射这一半反倒是 LTC 也只能逼近的。

// 把一个点夹到矩形上，返回世界坐标。
//
// 高光的代表点靠它：先求出「理想反射方向和光平面的交点」，
// 再夹进矩形的边界里。
fn light_rect_closest(light: Light, point: vec3<f32>) -> vec3<f32> {
    let forward = normalize(light.direction.xyz);
    let right = normalize(light.right.xyz);
    let up = cross(forward, right);

    let offset = point - light.position.xyz;
    let u = clamp(dot(offset, right), -light.params.x, light.params.x);
    let v = clamp(dot(offset, up), -light.params.y, light.params.y);
    return light.position.xyz + right * u + up * v;
}

// 矩形对着色点张成的立体角乘以余弦，也就是漫反射的形状因子。
//
// 做法是把矩形的四条边看成四段弧：每条边贡献
// `acos(dot(v_i, v_j)) * dot(cross(v_i, v_j), n)`，四条加起来除以 2π。
// 这是多边形光源的经典闭式解，**精确**而不是近似。
fn light_rect_form_factor(light: Light, world_position: vec3<f32>, n: vec3<f32>) -> f32 {
    let forward = normalize(light.direction.xyz);
    let right = normalize(light.right.xyz);
    let up = cross(forward, right);
    let half = vec2<f32>(light.params.x, light.params.y);

    // 四个角，逆时针（从正面看）。顺序反了形状因子会是负的，
    // 结果是整块面板变成「吸光」的黑洞。
    let center = light.position.xyz;
    var corners = array<vec3<f32>, 4>(
        center - right * half.x - up * half.y,
        center + right * half.x - up * half.y,
        center + right * half.x + up * half.y,
        center - right * half.x + up * half.y,
    );

    // 背面不发光：着色点在板子背后时直接返回 0。
    if (dot(world_position - center, forward) <= 0.0) {
        return 0.0;
    }

    var sum = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        let a = normalize(corners[i] - world_position);
        let b = normalize(corners[(i + 1) % 4] - world_position);
        // `acos` 的定义域是 [-1,1]，浮点误差会越界，夹一下。
        let angle = acos(clamp(dot(a, b), -1.0, 1.0));
        sum += angle * dot(normalize(cross(a, b)), n);
    }
    // 除以 2π 归一化。负值意味着矩形整个在表面背后。
    return max(sum / 6.2831853, 0.0);
}

// 半球光的环境项：按法线在地面色和天空色之间插值。
//
// `n.y * 0.5 + 0.5` 把法线的竖直分量映到 0..1：朝下取地面色，
// 朝上取天空色，水平方向取两者的中间。
fn light_hemisphere(light: Light, normal: vec3<f32>) -> vec3<f32> {
    let t = normal.y * 0.5 + 0.5;
    return mix(light.params.rgb, light.color.rgb, t) * light.color.a;
}

// 这盏灯照不照亮这个物体。
//
// 两边都得同意：任一方把对方的层关掉就不照。用「与非零」而不是「相等」，
// 是为了让一盏灯能同时照好几层——相等的话每盏灯只能属于一层，
// 「照亮角色和道具、但不照场景」就写不出来了。
fn light_affects(light: Light, object_mask: u32) -> bool {
    return (light.extra.x & object_mask) != 0u;
}

// 光源的辐射亮度（颜色 × 强度 × 衰减）。
fn light_radiance(light: Light, attenuation: f32) -> vec3<f32> {
    return light.color.rgb * light.color.a * attenuation;
}
