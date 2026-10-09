// 海面材质钩子（kcomponents::ocean）。
//
// 数据从哪来：
//   custom_texture_array  六层：每个级联两层——位移（高度 16 位拆两字节 + 水平位移 x/z）、
//                         坡度 x/z + 泡沫。CPU 每帧做完 FFT 编码进来。
//   custom_texture0       调色板（16×1）：水色、吸收、散射、泡沫色、雾色、太阳 / 天空辐射、太阳方向……
//   custom_texture1       尾迹图（以某点为中心的一块世界空间区域）：r = 白沫，g = 起伏高度
//   base_color_texture / custom_texture2 / custom_texture3：三层泡沫（白浪、表面薄沫、岸边）的纹理
//   normal_texture        水深图
//   params[0]  = (网格中心 x, 网格中心 z, 纹素数 N, 有泡沫贴图 0/1)
//   params[1]  = (三个级联的平铺边长, _)
//   params[2]  = (三个级联的位移编码范围, 坡度编码范围)
//   params[3]  = (尾迹图中心 x, 中心 z, 边长, 尾迹开关)
//
// 顶点阶段只做位移；法线、泡沫、颜色全在片元阶段逐像素采样——网格稀的远处照样有细节。

fn ocean_palette(index: i32) -> vec4<f32> {
    return textureSampleLevel(custom_texture0, base_color_sampler, vec2<f32>((f32(index) + 0.5) / 16.0, 0.5), 0.0);
}

fn ocean_uv(world_xz: vec2<f32>, tile: f32, texels: f32) -> vec2<f32> {
    // CPU 网格的第 i 个采样点在 i·L/N 处，GPU 纹素中心在 (i+0.5)/N 处：挪半个纹素对齐。
    return world_xz / tile + vec2<f32>(0.5 / texels);
}

// 位移：xyz = (dx, 高度, dz)，米。
fn ocean_displacement(cascade: i32, uv: vec2<f32>, range: f32) -> vec3<f32> {
    let t = textureSampleLevel(custom_texture_array, base_color_sampler, uv, cascade * 2, 0.0);
    // 高度 16 位：高字节在 r、低字节在 g。拆成两个字节各自双线性插值再合起来仍然正确——
    // 合成是线性的，插值和线性组合可交换。
    let height = (t.r * 65280.0 + t.g * 255.0) / 65535.0 * 2.0 - 1.0;
    return vec3<f32>(t.b * 2.0 - 1.0, height, t.a * 2.0 - 1.0) * range;
}

// 坡度 (∂h/∂x, ∂h/∂z) 与泡沫。
fn ocean_slope_foam(cascade: i32, uv: vec2<f32>, slope_range: f32) -> vec3<f32> {
    let t = textureSampleLevel(custom_texture_array, base_color_sampler, uv, cascade * 2 + 1, 0.0);
    return vec3<f32>((t.r * 2.0 - 1.0) * slope_range, (t.g * 2.0 - 1.0) * slope_range, t.b);
}

// ── 水深图（材质的法线贴图槽）：浅水里浪变矮、拍岸浪 ──
fn ocean_depth(world_xz: vec2<f32>, params: MaterialParams) -> f32 {
    let extent = params[1].w;
    if (extent <= 0.0) {
        return 1000.0;
    }
    let c = textureSampleLevel(custom_texture0, base_color_sampler, vec2<f32>(9.5 / 16.0, 0.5), 0.0);
    let center = vec2<f32>(c.r * 65280.0 + c.g * 255.0, c.b * 65280.0 + c.a * 255.0) - vec2<f32>(32768.0);
    let uv = (world_xz - center) / extent + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return 1000.0;
    }
    return textureSampleLevel(normal_texture, base_color_sampler, uv, 0.0).r * 32.0;
}

// 浅水里浪高打几折：和 CPU 的 shoal_factor 一致。
fn ocean_shoal(depth: f32) -> f32 {
    return 0.2 + 0.8 * smoothstep(0.5, 9.0, depth);
}

// 距离淡出：短波在远处采样不足会闪，在那之前就让它退场。
fn ocean_fade(distance: f32, start: f32, end: f32) -> f32 {
    return 1.0 - smoothstep(start, end, distance);
}

fn ocean_vertex_fades(distance: f32) -> vec3<f32> {
    return vec3<f32>(1.0, ocean_fade(distance, 700.0, 1800.0), ocean_fade(distance, 60.0, 220.0));
}

fn ocean_normal_fades(distance: f32) -> vec3<f32> {
    return vec3<f32>(ocean_fade(distance, 4000.0, 12000.0), ocean_fade(distance, 450.0, 2200.0), ocean_fade(distance, 45.0, 180.0));
}

fn ocean_total_displacement(world_xz: vec2<f32>, params: MaterialParams, fades: vec3<f32>) -> vec3<f32> {
    let texels = params[0].z;
    var d = vec3<f32>(0.0);
    for (var i = 0; i < 3; i = i + 1) {
        if (fades[i] > 0.001) {
            d += ocean_displacement(i, ocean_uv(world_xz, params[1][i], texels), params[2][i]) * fades[i];
        }
    }
    return d;
}

fn material_vertex(vertex: VertexSurface) -> VertexSurface {
    var out = vertex;
    // 网格跟着相机平移（节点的平移 = params[0].xy），这里加回去得到世界坐标——
    // 波浪按世界坐标采样，网格怎么挪浪都不会跟着「游」。
    let world_xz = vertex.position.xz + vertex.params[0].xy;
    let distance = length(vertex.position.xz);
    let d = ocean_total_displacement(world_xz, vertex.params, ocean_vertex_fades(distance));
    out.position = vertex.position + d * ocean_shoal(ocean_depth(world_xz, vertex.params));
    // 尾迹的起伏（船压出来的 V 字尾浪、浮标晃出的波纹）直接叠上去。
    out.position.y += ocean_wake(world_xz, vertex.params).y;
    return out;
}

// 小噪声：泡沫的纹理、浅滩泡沫的破碎边缘。
fn ocean_hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn ocean_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = ocean_hash(i);
    let b = ocean_hash(i + vec2<f32>(1.0, 0.0));
    let c = ocean_hash(i + vec2<f32>(0.0, 1.0));
    let d = ocean_hash(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// 有泡沫贴图时（params[0].w > 0.5，贴图在基础色槽里）用贴图：两层不同尺度、往不同方向漂。
// 三层泡沫各自的纹理。花边状：主纹理定形，细的一层只轻轻扰动——两层平均会把镂空的洞抹成一片灰。
// `scale` 是平铺边长（米）。三个函数分开写，是因为纹理绑定不能当参数传。
fn ocean_foam_whitecap(p: vec2<f32>, scale: f32, time: f32) -> f32 {
    let a = textureSample(base_color_texture, base_color_sampler, p / scale + vec2<f32>(time * 0.011, time * 0.007)).r;
    let b = textureSample(base_color_texture, base_color_sampler, p / (scale * 0.33) - vec2<f32>(time * 0.017, -time * 0.013)).r;
    return clamp(a * 0.8 + b * 0.3 - 0.05, 0.0, 1.0);
}

fn ocean_foam_surface(p: vec2<f32>, scale: f32, time: f32) -> f32 {
    let a = textureSample(custom_texture2, base_color_sampler, p / scale + vec2<f32>(-time * 0.006, time * 0.009)).r;
    let b = textureSample(custom_texture2, base_color_sampler, p / (scale * 0.37) + vec2<f32>(time * 0.012, time * 0.004)).r;
    return clamp(a * 0.8 + b * 0.3 - 0.05, 0.0, 1.0);
}

fn ocean_foam_shore(p: vec2<f32>, scale: f32, time: f32) -> f32 {
    let a = textureSample(custom_texture3, base_color_sampler, p / scale + vec2<f32>(time * 0.02, -time * 0.013)).r;
    let b = textureSample(custom_texture3, base_color_sampler, p / (scale * 0.41) - vec2<f32>(time * 0.03, time * 0.011)).r;
    return clamp(a * 0.8 + b * 0.3 - 0.05, 0.0, 1.0);
}

// 覆盖量当阈值切纹理：覆盖量小时只剩纹理最亮的几缕，越大越满；边缘软。
fn ocean_foam_cut(pattern: f32, coverage: f32) -> f32 {
    return smoothstep(1.0 - coverage, 1.0 - coverage * 0.4 + 0.2, pattern) * smoothstep(0.02, 0.25, coverage);
}

// ── 尾迹图：r = 白沫，g = 起伏高度（±1.5 米） ──
// 尾迹图的边长（纹素），和 Ocean::new 里的 WakeMap 一致。
const WAKE_TEXELS: f32 = 256.0;
fn ocean_wake(world_xz: vec2<f32>, params: MaterialParams) -> vec2<f32> {
    if (params[3].w < 0.5) {
        return vec2<f32>(0.0);
    }
    let uv = (world_xz - params[3].xy) / params[3].z + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return vec2<f32>(0.0);
    }
    let t = textureSampleLevel(custom_texture1, base_color_sampler, uv, 0.0);
    // 贴着图边淡出，图外和图内接得上。
    let edge = smoothstep(0.0, 0.08, min(min(uv.x, uv.y), min(1.0 - uv.x, 1.0 - uv.y)));
    return vec2<f32>(t.r, (t.g * 2.0 - 1.0) * 1.5) * edge;
}

fn ocean_foam_pattern(p: vec2<f32>, time: f32) -> f32 {
    // 两层不同尺度、往不同方向漂的噪声相乘：大块里套着细碎的气泡。
    let a = ocean_noise(p * 0.9 + vec2<f32>(time * 0.05, time * 0.03));
    let b = ocean_noise(p * 3.7 - vec2<f32>(time * 0.11, -time * 0.07));
    return clamp(a * 0.6 + b * 0.6, 0.0, 1.0);
}

// 雾（大气透视）：越远越接近天边的颜色。0 = 没雾，1 = 全是雾。
fn ocean_haze(distance: f32) -> f32 {
    // 指数平方：近处几乎不受影响，几百米外迅速变淡，到海平线处和天边的雾带接上。
    let density = ocean_palette(7).a * 0.0035;
    let x = max(distance - 150.0, 0.0) * density;
    return min(1.0 - exp(-x * x - x * 0.4), 0.97);
}

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    let params = surface.params;
    let texels = params[0].z;
    let camera = globals.camera_position.xyz;
    let distance = length(surface.world_position - camera);
    // 这个像素在海面上盖住多大一块（米，取长轴）：掠射角下沿视线方向会拉得很长。
    // 一级浪的纹素比它小，采样就会走样——远处一片压扁、发颤的假花纹（摩尔纹）。
    // 必须在任何分支之前求导数。
    let footprint = max(length(dpdx(surface.world_position.xz)), length(dpdy(surface.world_position.xz)));

    // ── 找回「这一点原本在哪」：顶点被水平推过，纹理是按推之前的坐标存的 ──
    // 迭代一次 p₀ = p − D(p) 就够近（位移比波长小得多）。
    var p0 = surface.world_position.xz;
    let vfades = ocean_vertex_fades(distance);
    let map_depth = ocean_depth(p0, params);
    let shoal = ocean_shoal(map_depth);
    p0 = p0 - ocean_total_displacement(p0, params, vfades).xz * shoal;

    // ── 坡度 → 法线，三层泡沫 ──
    // 按像素大小淡出：纹素比像素小的那一级退场，退掉的起伏变成粗糙度（远处是一片柔和的光泽，不是细花纹）。
    let fades = ocean_normal_fades(distance) * vec3<f32>(
        1.0 - smoothstep(1.5, 6.0, footprint / (params[1].x / texels)),
        1.0 - smoothstep(1.5, 6.0, footprint / (params[1].y / texels)),
        1.0 - smoothstep(1.5, 6.0, footprint / (params[1].z / texels)),
    );
    var slope = vec2<f32>(0.0);
    var whitecap = 0.0;
    var lost = 0.0;
    for (var i = 0; i < 3; i = i + 1) {
        let s = ocean_slope_foam(i, ocean_uv(p0, params[1][i], texels), params[2].w);
        slope += s.xy * fades[i];
        // 白浪主要来自大浪：涟漪那一级大风时几乎处处「翻卷」，按全权重算的话整片海都是白的。
        let foam_weight = select(select(1.0, 0.25, i == 1), 0.0, i == 2);
        whitecap = max(whitecap, s.z * max(fades[i], 0.35) * foam_weight);
        // 淡出去的那部分坡度变成粗糙度：远处的海不是镜面，是一片微小起伏的统计平均。
        lost += (1.0 - fades[i]) * (0.04 + f32(i) * 0.02);
    }
    // 尾迹的坡度：差分两个邻点。坡陡的地方（V 字尾浪的两条臂、浪头）会翻出白沫。
    var wake_steep = 0.0;
    if (params[3].w > 0.5) {
        let e = params[3].z / WAKE_TEXELS;
        let wake_here = ocean_wake(surface.world_position.xz, params).y;
        let wake_x = ocean_wake(surface.world_position.xz + vec2<f32>(e, 0.0), params).y;
        let wake_z = ocean_wake(surface.world_position.xz + vec2<f32>(0.0, e), params).y;
        let wake_slope = vec2<f32>(wake_x - wake_here, wake_z - wake_here) / e;
        slope += wake_slope;
        wake_steep = length(wake_slope);
    }
    var normal = normalize(vec3<f32>(-slope.x, 1.0, -slope.y));
    // 从水下看还是从水上看，整帧只有一个答案：相机在不在水里（CPU 按相机处的浪高算好，调色板第 15 格）。
    // 不能逐像素拿法线朝向判断——掠射角下远处浪的背坡法线背着相机，会被当成「从水下看」，
    // 画成暗的水下颜色、没有反射，远处一片暗色的细碎花纹。
    let below = ocean_palette(15).r > 0.5;
    if (below) {
        // 水下：水面的背面朝着相机。
        normal = -normal;
    }
    // 法线背着相机的那点（水上看到浪的背坡、水下看到浪的正面）往视线方向掰一点：
    // 真实世界里这一小块会被前面的浪挡住，这里让它和旁边接得上。
    let facing = dot(normal, surface.view_direction);
    if (facing < 0.02) {
        normal = normalize(normal + surface.view_direction * (0.02 - facing));
    }

    let deep_absorb = ocean_palette(0).rgb * 0.6;
    let scatter = ocean_palette(1).rgb;
    let shallow = ocean_palette(2).rgb;
    let foam_color = ocean_palette(3).rgb;
    let sun_radiance = ocean_palette(5).rgb * 8.0;
    let sky_radiance = ocean_palette(6).rgb * 4.0;
    let sun_dir = normalize(ocean_palette(7).rgb * 2.0 - 1.0);
    let look = ocean_palette(8);
    let foam_strength = look.r * 2.0;
    let refraction_strength = look.b * 0.15;

    // ── 屏幕空间折射 + 按水深吸收 ──
    let offset = normal.xz * refraction_strength * clamp(20.0 / max(distance, 1.0), 0.15, 1.0);
    var uv = surface.screen_uv + offset;
    if (scene_depth(uv) < surface.view_depth) {
        uv = surface.screen_uv;
    }
    let behind = scene_color(uv);
    let depth_below = max(scene_depth(uv) - surface.view_depth, 0.0);
    // 沿视线走过的水越多吸收越多；颜色按波长分别吸收——红光先没，所以深处偏蓝。
    let transmittance = exp(-deep_absorb * depth_below);
    // 浅滩（底下有东西、而且不深）偏青绿：沙底反上来的光。
    // 清澈的热带浅水，十几米深的沙底照样把水染成青绿。
    // 随水深指数变淡（几米内看得清底，十来米就只剩一点影子），没有一圈硬边。
    let shallow_mix = exp(-depth_below * 0.2);
    let inscatter_color = mix(scatter, shallow, shallow_mix * 0.7);
    // 水体里散射出来的光：天光 + 一部分直射阳光（被水里的颗粒散到四面八方）。
    let ambient_light = sky_radiance * 0.6 + sun_radiance * max(sun_dir.y, 0.0) * 0.3;

    // 次表面散射：迎着太阳看浪尖时浪是透亮的。浪越高越亮。
    let height = ocean_total_displacement(p0, params, vec3<f32>(1.0, fades.y, 0.0)).y;
    // 透射：浪尖离平均海面越高、水越薄，阳光从背后透过来就越亮；只有一层淡淡的青绿。
    let crest_t = clamp(height / max(params[2].x + params[2].y, 0.1) + 0.15, 0.0, 1.0);
    let sss = pow(max(dot(-surface.view_direction, sun_dir) * 0.5 + 0.5, 0.0), 4.0)
        * crest_t * crest_t * 0.06 * max(sun_dir.y + 0.15, 0.0);
    let inscatter = inscatter_color * (ambient_light + sun_radiance * sss) * (1.0 - transmittance);
    var water = behind * transmittance + inscatter;

    // ── 泡沫：三层，各有纹理、颜色、覆盖量（调色板 11–14） ──
    let wind = ocean_palette(10).rg * 2.0 - 1.0;
    let along = normalize(select(vec2<f32>(1.0, 0.0), wind, length(wind) > 0.1));
    let across = vec2<f32>(-along.y, along.x);
    let layer_whitecap = ocean_palette(11);
    let layer_surface = ocean_palette(12);
    let layer_shore = ocean_palette(13);
    let scales = max(ocean_palette(14).rgb * 32.0, vec3<f32>(0.5));
    // 白浪挂在浪峰上，浪峰和风向垂直：纹理沿浪峰（横风方向）拉长。表面薄沫被风吹成顺风的条纹。
    let crest_space = vec2<f32>(dot(p0, along), dot(p0, across) * 0.45);
    let wind_space = vec2<f32>(dot(p0, along) * 0.5, dot(p0, across));
    var pattern_whitecap = ocean_foam_pattern(crest_space / scales.x * 4.0, surface.time);
    var pattern_surface = ocean_foam_pattern(wind_space / scales.y * 4.0, surface.time);
    var pattern_shore = ocean_foam_pattern(p0 / scales.z * 4.0, surface.time);
    if (params[0].w > 0.5) {
        pattern_whitecap = ocean_foam_whitecap(crest_space, scales.x, surface.time);
        pattern_surface = ocean_foam_surface(wind_space, scales.y, surface.time);
        pattern_shore = ocean_foam_shore(p0, scales.z, surface.time);
    }

    // ① 白浪：只在真正翻卷的大浪浪尖上。
    // 翻卷看最大那一级（波长 19 米以上，含主浪）的雅可比：跌破阈值处就是浪面折叠、浪尖破碎。
    // 风浪那一级（3–19 米）大风时到处都在「折叠」，拿它出白浪就是满海碎点。模糊四邻点，破碎区是一整片。
    let texel = params[1].x / texels;
    var breaking = 0.0;
    var residual = 0.0;
    for (var k = 0; k < 4; k = k + 1) {
        let offset = vec2<f32>(f32(k % 2) - 0.5, f32(k / 2) - 0.5);
        breaking += ocean_slope_foam(0, ocean_uv(p0 + offset * texel, params[1].x, texels), params[2].w).z;
        // 表面薄沫看得更宽：白浪过后留下的那片沫子会散开。
        residual += ocean_slope_foam(0, ocean_uv(p0 + offset * texel * 4.0, params[1].x, texels), params[2].w).z;
    }
    breaking = breaking * 0.25;
    residual = residual * 0.25;
    // 而且必须在浪峰上：这一点高出平均海面（只看大浪那一级的高度，浪谷里不会有白浪）。
    let crest_height = ocean_displacement(0, ocean_uv(p0, params[1].x, texels), params[2].x).y;
    let on_crest = smoothstep(0.05, 0.35, crest_height / max(params[2].x, 0.1));
    let master = min(foam_strength * 1.3, 1.0);
    let cover_whitecap = smoothstep(0.05, 0.55, max(breaking, whitecap * 0.5)) * on_crest * master * layer_whitecap.a * 2.0;
    let foam_whitecap = ocean_foam_cut(pattern_whitecap, clamp(cover_whitecap, 0.0, 1.0)) * (0.6 + min(cover_whitecap, 1.0) * 0.4);

    // ② 表面薄沫：白浪过后留在水面上的一层，泡沫图里慢慢衰减的那部分。浪峰过去以后它还在，
    // 所以不要求在浪峰上；很薄、半透明，跟着风拉成条。
    let cover_surface = smoothstep(0.02, 0.45, max(residual, whitecap * 0.35)) * (1.0 - on_crest * 0.5) * master * layer_surface.a * 2.0 * 0.6;
    let foam_surface = ocean_foam_cut(pattern_surface, clamp(cover_surface, 0.0, 1.0)) * 0.5;

    // ③ 岸边和物体：岸线、拍岸浪、物体周围（船身、礁石——水面离背后的东西很近的地方）、尾迹。
    var cover_shore = 0.0;
    // 物体和岸线贴着水的那一圈（几十厘米）：再宽就成了一大片白斑。
    let contact = (1.0 - smoothstep(0.0, 0.55, depth_below)) * step(0.01, depth_below);
    cover_shore = max(cover_shore, contact * (0.75 + 0.25 * sin(surface.time * 1.3 + p0.x * 0.4)));
    // 拍岸浪：沿等深线的一道道白线，相位随时间减小——白线一圈圈往岸边推，到最浅处散掉。
    if (map_depth < 6.0) {
        // 相位里掺一点低频噪声，白线才是断断续续、起伏的，而不是描出来的等深线。
        let wobble = ocean_noise(p0 * 0.08 + vec2<f32>(surface.time * 0.05, 0.0)) * 3.0;
        let wave = sin(map_depth * 1.5 + surface.time * 1.4 + wobble);
        let band = smoothstep(0.55, 1.0, wave) * (1.0 - smoothstep(0.8, 5.0, map_depth)) * smoothstep(0.05, 0.5, map_depth);
        cover_shore = max(cover_shore, band * 0.75);
    }
    // 尾迹的白沫：船尾翻出来的那一道，加上尾浪坡陡处翻起的白线。
    cover_shore = max(cover_shore, ocean_wake(surface.world_position.xz, params).x * 0.9);
    cover_shore = max(cover_shore, smoothstep(0.08, 0.3, wake_steep) * 0.7);
    cover_shore = clamp(cover_shore * layer_shore.a * 2.0, 0.0, 1.0);
    let foam_shore = ocean_foam_cut(pattern_shore, cover_shore) * (0.65 + cover_shore * 0.35);

    // 叠起来：总量按「1 − 都没盖住」合成，颜色按各层的量加权。
    let foam = clamp(1.0 - (1.0 - foam_whitecap) * (1.0 - foam_surface) * (1.0 - foam_shore), 0.0, 1.0);
    let foam_weights = foam_whitecap + foam_surface + foam_shore + 1e-4;
    let foam_tint = foam_color * (layer_whitecap.rgb * foam_whitecap + layer_surface.rgb * foam_surface + layer_shore.rgb * foam_shore) / foam_weights;

    // 透明度：浅处能看清底，深处只剩水色。
    let opacity = 1.0;

    // ── 交给引擎的光照：水本身没有漫反射，只有菲涅耳镜面；颜色全在自发光里 ──
    // 和光照钩子里用同一个距离（视空间深度），三处的雾才对得上。
    let haze = ocean_haze(surface.view_depth);
    // 和天穹的地平线雾带同一个颜色（天色亮度、偏灰蓝），海天接缝处看不出断层。
    let horizon = ocean_palette(4).rgb * 8.0;
    let haze_color = mix(horizon, vec3<f32>(dot(horizon, vec3<f32>(0.2126, 0.7152, 0.0722))) * vec3<f32>(0.85, 0.93, 1.05), 0.6) * 0.62;
    let fresnel_view = 0.02 + 0.98 * pow(1.0 - max(dot(normal, surface.view_direction), 0.0), 5.0);
    var emissive = water * (1.0 - fresnel_view) * (1.0 - foam);
    if (below) {
        // 从水下往上看：斯涅尔窗——只有偏离竖直方向 48.6° 以内的视线能穿出水面看到天，
        // 窗外是全反射回来的水下颜色（偏暗的青蓝），窗边有一圈亮。
        let cos_view = max(dot(normal, surface.view_direction), 0.0);
        let window = smoothstep(0.62, 0.72, cos_view);
        let rim = smoothstep(0.55, 0.66, cos_view) * (1.0 - window);
        let reflected = scatter * ambient_light * 0.75;
        emissive = mix(reflected, behind * 0.7, window) + scatter * ambient_light * rim * 0.6;
    }
    out.emissive = mix(emissive, haze_color, haze);
    out.base_color = vec4<f32>(foam_tint * foam, opacity);
    out.normal = normal;
    out.metallic = 0.0;
    out.roughness = clamp(look.g * 0.25 + lost + foam * 0.8, 0.02, 1.0);
    // 遮蔽当标记用：从水下看时置 0，光照和环境光钩子据此不加天空反射和高光——
    // 背面朝着相机、掠射角下菲涅耳接近 1，不拦的话浪的下表面白成一片。
    out.occlusion = select(1.0, 0.0, below);
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    let lit = pbr_direct_lighting(
        (*surface).normal, (*surface).view_direction, input.light_direction,
        (*surface).base_color.rgb, (*surface).metallic, (*surface).roughness,
        input.radiance,
    );
    if ((*surface).occlusion < 0.5) {
        return vec3<f32>(0.0);
    }
    // 波光粼粼：FFT 最细的一级也有几十厘米，太阳的倒影还是一大片糊的。再叠一层更碎的、
    // 跟着时间抖动的微法线，配一个极窄的高光——只有恰好朝向太阳的小面一闪一闪地亮。
    let p = (*surface).world_position.xz * 2.7;
    let t = (*surface).time;
    let jitter = vec2<f32>(
        ocean_noise(p + vec2<f32>(t * 1.1, t * 0.5)) + ocean_noise(p * 2.3 - vec2<f32>(t * 1.7, -t * 0.9)) * 0.5,
        ocean_noise(p.yx + vec2<f32>(13.0 - t * 0.8, t * 1.3)) + ocean_noise(p.yx * 2.1 + vec2<f32>(t * 1.5, 31.0)) * 0.5,
    ) - vec2<f32>(0.75);
    // 抖得很轻：闪光只聚在太阳倒影那一条上，别的地方不闪。
    let micro = normalize((*surface).normal + vec3<f32>(jitter.x, 0.0, jitter.y) * 0.14);
    let half_vector = normalize(input.light_direction + (*surface).view_direction);
    let glint = pow(max(dot(micro, half_vector), 0.0), 2200.0) * ocean_fade((*surface).view_depth, 40.0, 600.0)
        // 粗估像素大小（钩子里不能求导）：掠射角下一个像素盖住的海面比 0.3 米大，闪光就会变成闪烁的噪点。
        * (1.0 - smoothstep(0.1, 0.3, (*surface).view_depth * 0.0012 / max(abs((*surface).view_direction.y), 0.02)))
        * (1.0 - clamp((*surface).base_color.r * 2.0, 0.0, 1.0));
    return (lit + input.radiance * glint * 3.0) * (1.0 - ocean_haze((*surface).view_depth));
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    if ((*surface).occlusion < 0.5) {
        return vec3<f32>(0.0);
    }
    // 镜面那份是天空的反射（环境图是天空烘出来的）。屏幕空间反射命中的地方换成场景里的东西——
    // 船、岛、礁石倒映在水面上；没命中（射出屏幕、射向天空）的地方仍是天空。
    var specular = input.specular * 1.1;
    let strength = ocean_palette(14).a;
    if (strength > 0.01) {
        let n = (*surface).normal;
        let v = (*surface).view_direction;
        let hit = ocean_ssr((*surface).world_position, reflect(-v, n));
        let fresnel = 0.02 + 0.98 * pow(1.0 - max(dot(n, v), 0.0), 5.0);
        specular = mix(specular, hit.rgb * fresnel, hit.a * strength);
    }
    return (input.diffuse + specular + input.hemisphere) * (1.0 - ocean_haze((*surface).view_depth));
}

// 屏幕空间反射：沿反射方向在世界里步进，每一步投到屏幕上，和深度缓冲比——
// 走到某个不透明物体「后面」一点点就算打中，取那里的场景颜色。
// 深度缓冲里只有不透明物体（水面自己在半透明那一趟），所以倒影里是船、岛、礁石。
// 返回 (颜色, 命中权重)。
fn ocean_ssr(origin: vec3<f32>, direction: vec3<f32>) -> vec4<f32> {
    // 朝下的反射（浪的背坡）打到的是水下，不管。
    if (direction.y < -0.05) {
        return vec4<f32>(0.0);
    }
    var t = 0.4;
    var last_t = 0.0;
    for (var i = 0; i < 28; i = i + 1) {
        let p = origin + direction * t;
        let clip = globals.view_proj * vec4<f32>(p, 1.0);
        if (clip.w <= 0.05) {
            break;
        }
        let ndc = clip.xy / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
            break;
        }
        let behind = clip.w - scene_depth(uv);
        // 厚度随步长放宽：远处一步跨得大。
        if (behind > 0.0 && behind < max(0.8, (t - last_t) * 1.5)) {
            // 二分细化几次，倒影的边缘才不是一格一格的。
            var lo = last_t;
            var hi = t;
            for (var j = 0; j < 4; j = j + 1) {
                let mid = (lo + hi) * 0.5;
                let c = globals.view_proj * vec4<f32>(origin + direction * mid, 1.0);
                let u = vec2<f32>(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5);
                if (c.w - scene_depth(u) > 0.0) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let c = globals.view_proj * vec4<f32>(origin + direction * hi, 1.0);
            let hit_uv = vec2<f32>(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5);
            // 贴着屏幕边、走得太远的命中淡出：倒影不会在屏幕边上一刀切断。
            let edge = smoothstep(0.0, 0.08, min(min(hit_uv.x, hit_uv.y), min(1.0 - hit_uv.x, 1.0 - hit_uv.y)));
            let fade = edge * (1.0 - smoothstep(60.0, 140.0, hi));
            return vec4<f32>(scene_color(hit_uv), fade);
        }
        last_t = t;
        t = t * 1.22 + 0.15;
    }
    return vec4<f32>(0.0);
}
