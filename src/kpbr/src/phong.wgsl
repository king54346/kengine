// Blinn-Phong：three.js 的 `MeshPhongMaterial` / `MeshLambertMaterial`。
//
// 和 three.js 的 BRDF 逐项对应（`BRDF_Lambert` + `BRDF_BlinnPhong`）：
//
//   漫反射 = 反照率 / π
//   高光   = F_Schlick(高光色, 1, v·h) × ¼ × D_BlinnPhong(光泽度, n·h)
//   D      = (光泽度 / 2 + 1) / π × (n·h)^光泽度
//
// 老式的经验模型，不守能量，也不按粗糙度收窄——但很多资产（OBJ 的 .mtl、
// 老游戏的材质）就是按它调的，换成 PBR 会显得发灰。
//
// params[0] = (高光色 rgb, 光泽度)
// params[1] = (高光贴图强度（0 = 不用；用 custom_texture0 的 r 乘在高光色上）,
//              棋盘格缩放（0 = 不用）, _, _)
// params[2].rgb / params[3].rgb = 棋盘格的两种高光色（three.js 例子里的
//              `mix(color(0x0000FF), color(0xFF0000), checker(uv * 5))`）

fn phong_specular_color(surface: ptr<function, Surface>) -> vec3<f32> {
    var specular = (*surface).params[0].rgb;
    let checker_scale = (*surface).params[1].y;
    if (checker_scale > 0.0) {
        let cell = floor((*surface).uv * checker_scale);
        let checker = abs(cell.x + cell.y) % 2.0;
        specular = mix((*surface).params[2].rgb, (*surface).params[3].rgb, checker);
    }
    let map_strength = (*surface).params[1].x;
    if (map_strength > 0.0) {
        let map = textureSample(custom_texture0, base_color_sampler, (*surface).uv).r;
        specular *= mix(1.0, map, map_strength);
    }
    return specular;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    let n = (*surface).normal;
    let l = input.light_direction;
    let v = (*surface).view_direction;
    let n_dot_l = max(dot(n, l), 0.0);
    if (n_dot_l <= 0.0) {
        return vec3<f32>(0.0);
    }
    let albedo = (*surface).base_color.rgb;
    let diffuse = albedo * (1.0 / 3.14159265);

    let h = normalize(l + v);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);
    let shininess = max((*surface).params[0].w, 1e-3);
    let specular_color = phong_specular_color(surface);
    let fresnel = specular_color + (vec3<f32>(1.0) - specular_color) * pow(1.0 - v_dot_h, 5.0);
    let distribution = (shininess * 0.5 + 1.0) / 3.14159265 * pow(n_dot_h, shininess);
    let specular = fresnel * 0.25 * distribution;

    return (diffuse + specular) * input.radiance * n_dot_l;
}

// 环境光：只要漫反射那一份和半球光。Phong 没有环境镜面反射
// （three.js 里得另挂 envMap 才有）。
fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return input.diffuse + input.hemisphere;
}
