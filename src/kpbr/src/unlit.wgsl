// 不受光：three.js 的 `MeshBasicMaterial` / `LineBasicMaterial`。
//
// 颜色（基础色 × 贴图 × 顶点色）整个挪进自发光，基础色清零，再把直射光
// 和环境光两个钩子都返回 0——于是灯怎么摆、有没有 IBL，看到的都是颜色本身。
// 雾和色调映射仍然照常作用在它身上（它们在钩子之后）。
//
// 不能只返回 0 光照而不挪自发光：那样基础色没人用，画出来是全黑。
// 也不能只挪自发光而不关光照：基础色清零之后光照项是 0 没错，但
// 金属度为 0 时 F0 仍有 0.04 的镜面反射，灯下会冒出一层灰白的高光。

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    out.emissive = surface.base_color.rgb;
    out.base_color = vec4<f32>(0.0, 0.0, 0.0, surface.base_color.a);
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}
