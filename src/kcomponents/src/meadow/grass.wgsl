// 草叶（kcomponents::meadow::grass_material）。
//
//   params[0] = (根部颜色 rgb, 亮度)
//   params[1] = (梢部颜色 rgb, 逆光强度)
//   params[2] = (逆光颜色 rgb, 逆光集中度)
//   params[3] = (风向 x, 风向 z, 风力, 风速)
//   params[4] = (风的频率, 湍流, 常驻倾斜, 逆光偏梢)
//   params[5] = (影子里的底, 漫反射的底, 环境光比例, _)
//
// 每根草叶是一个实例（`Node::with_instances`）：网格是一根宽 1、高 1 的叶片，实例矩阵带着位置、
// 朝向和宽高；实例颜色是这根的一点深浅。uv.y = 叶片上的高度 0..1。

fn material_vertex(vertex: VertexSurface) -> VertexSurface {
    var out = vertex;
    let p = vertex.params;
    // vertex.model 已经乘过实例矩阵：它的原点就是这根草的根（风的相位按它算，整根一起摆）。
    let root = vertex.model * vec4<f32>(0.0, 0.0, 0.0, 1.0);
    let tip = vertex.uv.y;
    let wind = meadow_wind(root.xz, p[3].xy, p[3].z, p[3].w, p[4].x, p[4].y, p[4].z, vertex.time);
    out.position += meadow_to_model(wind * tip * tip, vertex.model);
    return out;
}

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    let p = surface.params;
    let gradient = smoothstep(0.08, 1.0, surface.uv.y);
    // base_color 进来时是 材质基础色（白）× 实例颜色，也就是这片叶子的深浅。
    let color = mix(p[0].rgb, p[1].rgb, gradient) * surface.base_color.rgb * p[0].w;
    out.base_color = vec4<f32>(color, 1.0);
    // 光照按正上方算：叶片的真实朝向是随机的，拿它算漫反射会闪。
    out.normal = vec3<f32>(0.0, 1.0, 0.0);
    out.metallic = 0.0;
    out.roughness = 1.0;
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    let p = (*surface).params;
    let direct = meadow_direct(surface, input, p[5].x, p[5].y);
    let back = meadow_backlight(surface, input, (*surface).uv.y, p[1].w, p[2].w, p[4].w);
    return direct + p[2].rgb * input.radiance * back * p[0].w;
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    // 没有镜面：草不反光。
    return (input.diffuse + input.hemisphere) * (*surface).params[5].z;
}
