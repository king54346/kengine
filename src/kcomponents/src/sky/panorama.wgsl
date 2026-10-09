// 全景天空（kcomponents::sky 的全景模式）：天穹直接贴一张等距柱状投影的天空照片。
//
//   base_color_texture  全景图（sRGB，硬件解码成线性）
//   params[0] = (亮度倍数, 绕竖轴旋转（弧度）, 地平线以下压暗, 地平线雾 0–1)
//
// 映射和 kpbr::hdr::HdrImage::sample_direction 一致：u = (atan2(z, x) + π) / τ，v = acos(y) / π。
// 环境光是同一张图烘出来的，天上看到的和水面反射里的是同一片天。

const PANORAMA_PI: f32 = 3.14159265;

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    var dir = normalize(surface.world_position - globals.camera_position.xyz);
    let yaw = surface.params[0].y;
    let c = cos(yaw);
    let s = sin(yaw);
    dir = vec3<f32>(c * dir.x - s * dir.z, dir.y, s * dir.x + c * dir.z);
    // 地平线以下取贴着地平线的那一圈（照片下半部常是地面或空白），往下渐暗。
    let below = min(dir.y, 0.0);
    let sample_dir = normalize(vec3<f32>(dir.x, max(dir.y, 0.004), dir.z));
    let u = (atan2(sample_dir.z, sample_dir.x) + PANORAMA_PI) / (2.0 * PANORAMA_PI);
    let v = acos(clamp(sample_dir.y, -1.0, 1.0)) / PANORAMA_PI;
    var color = textureSampleLevel(base_color_texture, base_color_sampler, vec2<f32>(u, v), 0.0).rgb;
    color *= surface.params[0].x * (1.0 + below * surface.params[0].z);
    // 地平线雾带：和程序化天空一样，贴着海平线一道灰蓝。
    let lum = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let band = exp(-max(dir.y, 0.0) * 22.0) * clamp(surface.params[0].w * 1.4, 0.0, 0.6);
    color = mix(color, mix(color, vec3<f32>(lum) * vec3<f32>(0.85, 0.93, 1.05), 0.6) * 0.8, band);
    out.emissive = color;
    out.base_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    out.metallic = 0.0;
    out.roughness = 1.0;
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}
