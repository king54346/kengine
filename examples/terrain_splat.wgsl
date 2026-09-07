// 地形 splat：把最多 4 层地表贴图按权重图混合。
//
// `custom_texture0` 是 `SplatMap::to_texture()` 产出的权重图——RGBA 四个
// 通道分别是第 0~3 层在这个像素上的混合权重，已经归一化（四个通道加起来
// 恒为 1）。`custom_texture_array` 是四层地表贴图叠成的纹理数组，
// 层号 0~3 对应权重图的 R/G/B/A。两者由 `kscene::Scene::update` 和调用方
// 分别维护：权重图随笔刷涂改自动重传，贴图数组是静态资源、建材质时设一次。
//
// 实际层数不足 4 时，调用方把多出的数组层填成随便哪一层的复制品——
// `SplatMap::to_texture()` 已经把那些通道的权重钉死成 0，乘 0 之后
// 采到什么都不影响最终颜色，这里不必为层数是否够 4 专门分支。

fn material_surface(surface: Surface) -> Surface {
    var out = surface;

    let weights = textureSample(custom_texture0, base_color_sampler, surface.uv);

    let layer0 = textureSample(custom_texture_array, base_color_sampler, surface.uv, 0);
    let layer1 = textureSample(custom_texture_array, base_color_sampler, surface.uv, 1);
    let layer2 = textureSample(custom_texture_array, base_color_sampler, surface.uv, 2);
    let layer3 = textureSample(custom_texture_array, base_color_sampler, surface.uv, 3);

    let blended = layer0.rgb * weights.r
        + layer1.rgb * weights.g
        + layer2.rgb * weights.b
        + layer3.rgb * weights.a;

    out.base_color = vec4<f32>(blended, surface.base_color.a);
    out.metallic = 0.0;
    out.roughness = 0.9;
    return out;
}
