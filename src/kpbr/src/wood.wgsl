// 程序化木纹：three.js `WoodNodeMaterial`（Blender 木纹节点图的 TSL 移植）一个函数一个函数照搬。
//
// 输入是**模型空间**坐标（加上一个平移，让同一块板子的不同实例纹理不同）：顶点钩子把它塞进
// `uv` / `uv1.x` 传给片元。木头的「年轮轴」是模型空间的 Z 轴。
//
// 参数在 params[8..15]（params[0..4] 是叠在上面的物理材质用的，清漆就靠它）：
//   8  = (centerSize, largeWarpScale, largeGrainStretch, smallWarpStrength)
//   9  = (smallWarpScale, fineWarpStrength, fineWarpScale, ringThickness)
//   10 = (ringBias, ringSizeVariance, ringVarianceScale, barkThickness)
//   11 = (splotchScale, splotchIntensity, cellScale, cellSize)
//   12 = (深色木纹 rgb, 清漆压暗系数)
//   13 = (浅色木纹 rgb, -)
//   14 = (纹理平移 xyz, -)
//
// 依赖 `kshader::noise`（mx_noise_float / mx_noise_vec3）。

const WOOD_PI2: f32 = 6.283185307179586;

// Blender 的 Map Range（线性）。`clamp` 照 TSL 版本写：先夹上界再夹下界——
// to_max < to_min 时结果恒为 to_min，原版就是这样，照搬以保持同样的图案。
fn wood_map_range(x: f32, from_min: f32, from_max: f32, to_min: f32, to_max: f32, clamp_result: bool) -> f32 {
    let factor = (x - from_min) / (from_max - from_min);
    let result = to_min + factor * (to_max - to_min);
    return select(result, max(min(result, to_max), to_min), clamp_result);
}

fn wood_hash3d(p: vec3<f32>) -> vec3<f32> {
    var p3 = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.xxy + p3.yzz) * p3.zyx);
}

fn wood_voronoi3d(x: vec3<f32>, smoothness: f32, randomness: f32) -> f32 {
    let p = floor(x);
    let f = fract(x);
    var res = 0.0;
    var total_weight = 0.0;
    for (var k = -1; k <= 1; k++) {
        for (var j = -1; j <= 1; j++) {
            for (var i = -1; i <= 1; i++) {
                let b = vec3<f32>(f32(i), f32(j), f32(k));
                let r = b - f + wood_hash3d(p + b) * randomness;
                let d = length(r);
                let weight = exp(-d * d / max(smoothness * smoothness, 0.001));
                res += d * weight;
                total_weight += weight;
            }
        }
    }
    if (total_weight > 0.0) {
        res /= total_weight;
    }
    return smoothstep(0.0, 1.0, res);
}

fn wood_soft_light(t: f32, col1: vec3<f32>, col2: vec3<f32>) -> vec3<f32> {
    let screen = vec3<f32>(1.0) - (vec3<f32>(1.0) - col2) * (vec3<f32>(1.0) - col1);
    return (1.0 - t) * col1 + t * ((vec3<f32>(1.0) - col1) * col2 * col1 + col1 * screen);
}

// 原版的 noiseFbm / noiseFbm3d 在这里只以 detail = 1、归一化的方式被调用：一层噪声映射到 [0, 1]。
fn wood_fbm(p: vec3<f32>) -> f32 {
    return mx_noise_float(p) * 0.5 + 0.5;
}

fn wood_fbm_2d(p: vec2<f32>) -> f32 {
    return mx_noise_float_2d(p) * 0.5 + 0.5;
}

fn wood_fbm3d(p: vec3<f32>) -> vec3<f32> {
    return mx_noise_vec3(p) * 0.5 + 0.5;
}

fn wood_center(p: vec3<f32>, center_size: f32) -> f32 {
    return wood_map_range(length(p.xy), 0.0, 1.0, 0.0, center_size, true);
}

fn wood_space_warp(p: vec3<f32>, warp_strength: f32, xy_scale: f32, z_scale: f32) -> vec3<f32> {
    let combined = vec3<f32>(xy_scale, xy_scale, z_scale) * p;
    let noise = (wood_fbm3d(combined * 2.4) - 0.5) * warp_strength;
    let p_xy = p * vec3<f32>(1.0, 1.0, 0.0);
    return noise * normalize(p_xy) + p_xy;
}

fn wood_rings(w: f32, ring_thickness: f32, ring_bias: f32, ring_size_variance: f32, ring_variance_scale: f32, bark_thickness: f32, view_distance: f32) -> f32 {
    // 原版把标量 w 喂给噪声，TSL 把它转成 vec2(w, w)，走二维 Perlin。
    let rings = fract((wood_fbm_2d(vec2<f32>(w * ring_variance_scale)) * ring_size_variance + w) * ring_thickness) * bark_thickness;
    let sharp = min(wood_map_range(rings, 0.0, ring_bias, 0.0, 1.0, true), wood_map_range(rings, ring_bias, 1.0, 1.0, 0.0, true));
    let blur = max(view_distance / 10.0, 1.0);
    return smoothstep(-blur, blur, sharp - 0.5) * 0.5 + 0.5;
}

fn wood_detail(warp: vec3<f32>, p: vec3<f32>, y: f32, splotch_scale: f32) -> f32 {
    let radial = clamp(atan2(warp.y, warp.x) / WOOD_PI2 + 0.5, 0.0, 1.0) * (WOOD_PI2 * 3.0);
    let combined = vec3<f32>(sin(radial), y, cos(radial) * p.z);
    return wood_fbm(vec3<f32>(0.1, 1.19, 0.05) * combined * splotch_scale);
}

fn wood_cells(p: vec3<f32>, cell_scale: f32, cell_size: f32) -> f32 {
    let warp = wood_space_warp(p * (cell_scale / 50.0), cell_scale / 1000.0, 0.1, 1.77);
    let cells = wood_voronoi3d(vec3<f32>(warp.xy * 75.0, 0.0), 0.5, 1.0);
    return wood_map_range(cells, cell_size, cell_size + 0.21, 0.0, 1.0, true);
}

fn wood_color(p: vec3<f32>, params: MaterialParams, view_distance: f32) -> vec3<f32> {
    let a = params[8];
    let b = params[9];
    let c = params[10];
    let d = params[11];
    let center = wood_center(p, a.x);
    let main_warp = wood_space_warp(wood_space_warp(p, center, a.y, a.z), a.w, b.x, 0.17);
    let detail_warp = wood_space_warp(main_warp, b.y, b.z, 0.17);
    let rings = wood_rings(length(detail_warp), 1.0 / b.w, c.x, c.y, c.z, c.w, view_distance);
    let detail = wood_detail(detail_warp, p, length(detail_warp), d.x);
    let cells = wood_cells(main_warp, d.z, d.w / max(view_distance * 10.0, 1.0));
    let base = mix(params[12].rgb, params[13].rgb, rings);
    return wood_soft_light(d.y, wood_soft_light(0.407, base, vec3<f32>(cells)), vec3<f32>(detail)) * params[12].w;
}

fn material_vertex(vertex: VertexSurface) -> VertexSurface {
    var out = vertex;
    let p = vertex.position + vertex.params[14].xyz;
    out.uv = p.xy;
    out.uv1 = vec2<f32>(p.z, 0.0);
    return out;
}

fn material_surface(s: Surface) -> Surface {
    var out = s;
    // 看得越远年轮越糊、细胞纹越淡（原版用 positionView 的长度；这里用视深度近似）。
    let p = vec3<f32>(s.uv, s.uv1.x);
    out.base_color = vec4<f32>(wood_color(p, s.params, s.view_depth), 1.0);
    return physical_surface(out);
}
