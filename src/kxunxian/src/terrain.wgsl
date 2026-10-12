// 寻仙地表分层混合（材质钩子）。照游戏自己的地形着色器（嵌在 `pge_core.dll` 里的 HLSL，
// `vsDetail` / `BlendTexLv1..4`）改写：
//
// - 平铺：游戏的贴图坐标是「以 16 米为一格」的地面坐标再乘每层的矩阵，`layer.xml` 的 `Scale`
//   就是每 16 米重复几次（草地 6 → 2.7 米一次）。
// - 混合：底层铺满，往上每层 `mix(下面, 这层, 这层贴图的 alpha × 权重)`。有几张草地贴图的 alpha 是
//   镂空的，混出来的边缘才是一簇一簇的，不能只看权重。
// - 明暗：顶点色（`color.dword`）乘在最后。
//
// 绑定：
// - `base_color_texture`：1×1 白图，于是 `surface.base_color` 正好是顶点色；它的采样器所有贴图共用。
// - `custom_texture_array`：整张地图用到的各层地表贴图，一层一张。
// - `custom_texture0..2`：这个单元的 12 个权重通道（RGBA × 3），排布和预混贴图一样，直接用网格的 uv。
// - `custom_texture3`：预混贴图，远处用。
// - `params[0..3]`：12 个通道各自的「数组层号 × 100 + Scale」，没用上是 -1。
// - `params[4]`：x = 底层的同样编码，y / z = 开始 / 完全换成预混贴图的距离（米）。

fn terrain_layer(code: f32, ground: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {
    let layer = floor(code / 100.0);
    let repeats = max(code - layer * 100.0, 0.01) / 16.0;
    // 用显式导数采样：循环里按通道跳过的分支不算统一控制流，`textureSample` 编不过。
    return textureSampleGrad(
        custom_texture_array,
        base_color_sampler,
        ground * repeats,
        i32(layer),
        ddx * repeats,
        ddy * repeats,
    );
}

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    var params = surface.params;
    // 原始数据是左手系，世界 z 取反回去，贴图的方向和游戏里一致。
    let ground = vec2<f32>(surface.world_position.x, -surface.world_position.z);
    let ddx = dpdx(ground);
    let ddy = dpdy(ground);
    var weights = array<vec4<f32>, 3>(
        textureSample(custom_texture0, base_color_sampler, surface.uv),
        textureSample(custom_texture1, base_color_sampler, surface.uv),
        textureSample(custom_texture2, base_color_sampler, surface.uv),
    );
    let preblend = textureSample(custom_texture3, base_color_sampler, surface.uv).rgb;

    var color = terrain_layer(params[4].x, ground, ddx, ddy).rgb;
    for (var i = 0; i < 12; i++) {
        let code = params[i / 4][i % 4];
        let weight = weights[i / 4][i % 4];
        if code < 0.0 || weight <= 0.0 {
            continue;
        }
        let layer = terrain_layer(code, ground, ddx, ddy);
        color = mix(color, layer.rgb, layer.a * weight);
    }

    // 远处换回预混贴图：平铺的重复感在远处很显眼，预混贴图本来就是给远处用的。
    let distance = length(surface.world_position - globals.camera_position.xyz);
    let far = smoothstep(params[4].y, params[4].z, distance);
    out.base_color = vec4<f32>(mix(color, preblend, far) * surface.base_color.rgb, 1.0);
    return out;
}
