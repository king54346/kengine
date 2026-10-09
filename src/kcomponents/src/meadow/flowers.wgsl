// 野花（kcomponents::meadow 的花卡片）。
//
//   params[0..12] = 四套配色，每套三个：花瓣 rgb、茎叶 rgb、花心 rgb
//   params[12]    = (风向 x, 风向 z, 风力, 风速)
//   params[13]    = (风的频率, 湍流, 常驻倾斜, 风的比例)
//   params[14]    = (亮度, 影子里的底, 漫反射的底, 环境光比例)
//
// custom_texture0 是三种花的遮罩图集：r = 花瓣、g = 茎叶、b = 花心、a = 覆盖。
// 每朵花是一个实例：instance_data.x = 图集里第几种花，.y = 第几套配色；实例颜色 = 一点色差。
// 网格是一张宽 1、高 1 的卡片，uv.u 0..1、uv.v = 0 在顶端。

fn material_vertex(vertex: VertexSurface) -> VertexSurface {
    var out = vertex;
    let p = vertex.params;
    // 卡片很窄，用顶点自己的 xz 当相位就行（和根部差不了几厘米）。
    let world = vertex.model * vec4<f32>(vertex.position, 1.0);
    let tip = 1.0 - vertex.uv.y;
    // 挪到图集里这一种花的那一格。
    out.uv = vec2<f32>((vertex.uv.x + vertex.instance_data.x) / 3.0, vertex.uv.y);
    let wind = meadow_wind(world.xz, p[12].xy, p[12].z, p[12].w, p[13].x, p[13].y, p[13].z, vertex.time);
    out.position += meadow_to_model(wind * tip * tip * p[13].w, vertex.model);
    return out;
}

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    let mask = textureSample(custom_texture0, base_color_sampler, surface.uv);
    if (mask.a < 0.28) {
        discard;
    }
    let palette = u32(clamp(surface.instance_data.y + 0.5, 0.0, 3.0)) * 3u;
    // 下标是运行时的：放进函数局部的 var 里再取（按值的数组动态取下标，有的后端不认）。
    var p = surface.params;
    // 色差主要落在花瓣上，茎叶和花心只沾一半。
    let tint = surface.base_color.rgb;
    let half_tint = mix(vec3<f32>(1.0), tint, 0.5);
    let color = mask.r * p[palette].rgb * tint
        + mask.g * p[palette + 1u].rgb * half_tint
        + mask.b * p[palette + 2u].rgb * half_tint;
    out.base_color = vec4<f32>(color * p[14].x, 1.0);
    out.normal = vec3<f32>(0.0, 1.0, 0.0);
    out.metallic = 0.0;
    out.roughness = 1.0;
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    let p = (*surface).params;
    return meadow_direct(surface, input, p[14].y, p[14].z);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return (input.diffuse + input.hemisphere) * (*surface).params[14].w;
}
