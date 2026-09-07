/// 供自定义材质使用的地形 splat 着色器钩子。
/// 
/// 这个文件会被包含到着色器中，以定义 `material_surface` 钩子。
/// 
/// 绑定约定：
/// - `custom_texture0`: Splat 权重纹理（一张覆盖全地形的 RGBA8 贴图，分别代表 4 层的权重）。
/// - `custom_texture_array`: 实际的材质贴图数组（至少 4 层）。

fn material_surface(surface: Surface) -> Surface {
    var out = surface;

    // splat_texture (权重) 使用了地形全局的 UV [0, 1]
    let weights = textureSample(custom_texture0, base_color_sampler, surface.uv);

    // 材质贴图使用世界坐标切片来获得重复的 UV
    // 假设缩放系数由 params[0].x 提供，如果没有则默认为 0.2
    var scale = 0.2;
    if out.params[0].x > 0.0 {
        scale = out.params[0].x;
    }
    let tile_uv = surface.world_position.xz * scale;

    let c0 = textureSample(custom_texture_array, base_color_sampler, tile_uv, 0);
    let c1 = textureSample(custom_texture_array, base_color_sampler, tile_uv, 1);
    let c2 = textureSample(custom_texture_array, base_color_sampler, tile_uv, 2);
    let c3 = textureSample(custom_texture_array, base_color_sampler, tile_uv, 3);

    out.base_color = c0 * weights.r + c1 * weights.g + c2 * weights.b + c3 * weights.a;

    return out;
}
