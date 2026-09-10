// Screen-space refraction, three-wavelength dispersion, thin-film interference
// and a grazing-angle sheen approximation. This is not a spectral path tracer.
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
    let specular = input.specular * mix(vec3<f32>(1.0), interference * 1.8, film);
    let sheen = (*s).params[2].rgb * pow(1.0 - nv, mix(5.0, 1.0, (*s).params[1].z)) * input.occlusion;
    return diffuse + specular + sheen;
}
