// Screen-space refraction, three-wavelength dispersion, thin-film interference
// and a grazing-angle sheen approximation. This is not a spectral path tracer.
//
// params[3] = (各向异性强度 [+2 表示有方向贴图], 各向异性旋转, 清漆强度, 清漆粗糙度)

// 各向异性的方向（世界空间）与强度。强度为 0 时返回的方向无意义。
fn physical_anisotropy(s: ptr<function, Surface>) -> vec4<f32> {
    var strength = (*s).params[3].x;
    var direction = vec2<f32>(1.0, 0.0);
    if (strength >= 1.5) {
        // KHR_materials_anisotropy：RG 是 [-1,1] 编码的切线空间方向，B 是强度倍率。
        let texel = textureSample(custom_texture0, base_color_sampler, (*s).uv).rgb;
        let encoded = texel.rg * 2.0 - 1.0;
        if (dot(encoded, encoded) > 1e-6) {
            direction = normalize(encoded);
        }
        strength = (strength - 2.0) * texel.b;
    }
    let angle = (*s).params[3].y;
    let c = cos(angle);
    let si = sin(angle);
    direction = vec2<f32>(c * direction.x - si * direction.y, si * direction.x + c * direction.y);
    let n = (*s).normal;
    // 切线跟着（可能被法线贴图改过的）法线重新正交化。
    let t0 = normalize((*s).tangent - n * dot(n, (*s).tangent));
    let b0 = cross(n, t0);
    return vec4<f32>(normalize(t0 * direction.x + b0 * direction.y), clamp(strength, 0.0, 1.0));
}

// 各向异性 GGX 的镜面项（已乘 n·l），按 glTF 规范的 D 与高度相关 V。
fn physical_anisotropic_specular(
    n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, t: vec3<f32>,
    strength: f32, roughness: f32, f0: vec3<f32>,
) -> vec3<f32> {
    let b = cross(n, t);
    let h = normalize(v + l);
    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);
    let alpha = roughness * roughness;
    let at = mix(alpha, 1.0, strength * strength);
    let ab = max(alpha, 1e-3);
    let t_dot_h = dot(t, h);
    let b_dot_h = dot(b, h);
    let a2 = at * ab;
    let f = vec3<f32>(ab * t_dot_h, at * b_dot_h, a2 * n_dot_h);
    let w2 = a2 / max(dot(f, f), 1e-7);
    let d = a2 * w2 * w2 / PBR_PI;
    let lambda_v = n_dot_l * length(vec3<f32>(at * dot(t, v), ab * dot(b, v), n_dot_v));
    let lambda_l = n_dot_v * length(vec3<f32>(at * dot(t, l), ab * dot(b, l), n_dot_l));
    let vis = 0.5 / max(lambda_v + lambda_l, 1e-7);
    let fresnel = pbr_fresnel_schlick(max(dot(h, v), 0.0), f0);
    return fresnel * d * vis * n_dot_l;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    let n = (*surface).normal;
    let v = (*surface).view_direction;
    let l = input.light_direction;
    let albedo = (*surface).base_color.rgb;
    let metallic = (*surface).metallic;
    let roughness = (*surface).roughness;

    var color: vec3<f32>;
    let aniso = physical_anisotropy(surface);
    if (input.light.position.w == LIGHT_RECT) {
        color = pbr_area_lighting(n, v, l, albedo, metallic, roughness, input.radiance, input.form_factor);
    } else if (aniso.w <= 0.0) {
        color = pbr_direct_lighting(n, v, l, albedo, metallic, roughness, input.radiance);
    } else {
        let n_dot_l = max(dot(n, l), 0.0);
        let f0 = pbr_f0(albedo, metallic);
        let h = normalize(v + l);
        let f = pbr_fresnel_schlick(max(dot(h, v), 0.0), f0);
        let diffuse = (vec3<f32>(1.0) - f) * (1.0 - metallic) * albedo / PBR_PI * n_dot_l;
        color = (diffuse + physical_anisotropic_specular(n, v, l, aniso.xyz, aniso.w, roughness, f0)) * input.radiance;
    }

    // 清漆：一层 IOR 1.5 的透明涂层，法线用几何法线（没有单独的清漆法线贴图）。
    let coat = (*surface).params[3].z;
    if (coat > 0.0) {
        let cn = (*surface).geometric_normal;
        let h = normalize(v + l);
        let n_dot_l = max(dot(cn, l), 0.0);
        let n_dot_v = max(dot(cn, v), 1e-4);
        let fc = pbr_fresnel_schlick(max(dot(h, v), 0.0), vec3<f32>(0.04)).x * coat;
        let cr = clamp((*surface).params[3].w, 0.03, 1.0);
        let d = pbr_distribution_ggx(max(dot(cn, h), 0.0), cr);
        let g = pbr_geometry_smith(n_dot_v, n_dot_l, cr);
        let coat_specular = d * g * fc / max(4.0 * n_dot_v * max(n_dot_l, 1e-4), 1e-7) * n_dot_l;
        color = color * (1.0 - fc) + vec3<f32>(coat_specular) * input.radiance;
    }
    return color;
}

// 用预滤波环境（没有 HDR 时退回程序化天空）采一个方向的镜面辐射亮度。
fn physical_environment(direction: vec3<f32>, roughness: f32) -> vec3<f32> {
    if (globals.ibl_params.x > 0.5) {
        return ibl_specular_prefiltered(prefiltered_env, prefiltered_sampler, 0.0, globals.ibl_params.x,
            direction, roughness, vec3<f32>(1.0), vec2<f32>(1.0, 0.0), globals.environment.sun_color.a);
    }
    return ibl_specular(globals.environment, direction, roughness, vec3<f32>(1.0), vec2<f32>(1.0, 0.0));
}
fn material_surface(s: Surface) -> Surface {
    var out = s;
    if (s.base_color.a < s.params[1].w) { discard; }
    if (s.params[2].w > 0.5) {
        out.emissive += s.base_color.rgb;
        out.base_color = vec4<f32>(0.0, 0.0, 0.0, s.base_color.a);
        out.metallic = 0.0;
    }
    return out;
}
fn material_ambient(s: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    if ((*s).params[2].w > 0.5) { return vec3<f32>(0.0); }
    let nv = clamp(dot((*s).normal, (*s).view_direction), 0.0, 1.0);
    let ior = max((*s).params[0].y, 1.0);
    let r0 = pow((ior - 1.0) / (ior + 1.0), 2.0);
    let f = r0 + (1.0 - r0) * pow(1.0 - nv, 5.0);
    var diffuse = input.diffuse + input.hemisphere;
    if ((*s).params[0].x > 0.0) {
        // Project the world-space refracted direction into camera space.
        let depth = max((*s).view_depth, 0.01);
        let thickness = (*s).params[0].z / depth;
        let spread = (*s).params[0].w * 0.025;
        let dr = refract(-(*s).view_direction, (*s).normal, 1.0 / max(ior - spread, 1.0));
        let dg = refract(-(*s).view_direction, (*s).normal, 1.0 / ior);
        let db = refract(-(*s).view_direction, (*s).normal, 1.0 / (ior + spread));
        // view-projection maps directions without translation (w=0).
        let pr = globals.view_proj * vec4<f32>(dr, 0.0);
        let pg = globals.view_proj * vec4<f32>(dg, 0.0);
        let pb = globals.view_proj * vec4<f32>(db, 0.0);
        let flip = vec2<f32>(0.5, -0.5) * thickness;
        let behind = vec3<f32>(scene_color((*s).screen_uv + pr.xy * flip).r,
            scene_color((*s).screen_uv + pg.xy * flip).g,
            scene_color((*s).screen_uv + pb.xy * flip).b) * (*s).base_color.rgb;
        diffuse = mix(diffuse, behind * (1.0 - f), (*s).params[0].x);
    }
    let film = (*s).params[1].x;
    let optical_path = 2.0 * 1.3 * (*s).params[1].y * sqrt(max(0.0, 1.0 - (1.0 - nv * nv) / (1.3 * 1.3)));
    let interference = 0.5 + 0.5 * cos(6.2831853 * optical_path / vec3<f32>(650.0, 510.0, 475.0));
    var specular = input.specular * mix(vec3<f32>(1.0), interference * 1.8, film);
    // 各向异性的环境反射：Filament 的「弯曲反射向量」——把采样法线朝
    // 各向异性方向掰，高光就沿那个方向拉长。按「弯过去 / 原方向」的
    // 亮度比缩放引擎算好的那份，这样菲涅尔和 BRDF 查找表的部分都保留。
    let aniso = physical_anisotropy(s);
    if (aniso.w > 0.0) {
        let n = (*s).normal;
        let v = (*s).view_direction;
        let r = (*s).roughness;
        let bitangent = cross(n, aniso.xyz);
        let anisotropic_tangent = cross(bitangent, v);
        let anisotropic_normal = normalize(cross(anisotropic_tangent, bitangent));
        let bend = aniso.w * clamp(5.0 * r, 0.0, 1.0);
        let bent = normalize(mix(n, anisotropic_normal, bend));
        let straight = physical_environment(reflect(-v, n), r);
        let curved = physical_environment(reflect(-v, bent), r);
        let ratio = (curved + vec3<f32>(1e-4)) / (straight + vec3<f32>(1e-4));
        specular *= clamp(ratio, vec3<f32>(0.0), vec3<f32>(8.0));
    }
    let sheen = (*s).params[2].rgb * pow(1.0 - nv, mix(5.0, 1.0, (*s).params[1].z)) * input.occlusion;
    var color = diffuse + specular + sheen;
    let coat = (*s).params[3].z;
    if (coat > 0.0) {
        let cn = (*s).geometric_normal;
        let cv = clamp(dot(cn, (*s).view_direction), 0.0, 1.0);
        let fc = (0.04 + 0.96 * pow(1.0 - cv, 5.0)) * coat;
        let coat_light = physical_environment(reflect(-(*s).view_direction, cn), clamp((*s).params[3].w, 0.03, 1.0));
        color = color * (1.0 - fc) + coat_light * fc * input.occlusion;
    }
    return color;
}
