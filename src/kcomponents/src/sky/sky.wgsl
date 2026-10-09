// 天穹材质钩子（kcomponents::sky）：大气散射 + 日盘 + 体积云 + 星星。
//
// 不受光照：颜色全放在自发光里，光照和环境光钩子返回 0。
//
//   params[0] = (太阳方向 xyz, 云量 0–1)
//   params[1] = (瑞利倍数, 米氏倍数, 太阳亮度, 云密度)
//   params[2] = (云的风偏移 x, z, 云步数, 夜光亮度)
//   params[3] = (相机海拔, 日盘角半径（度）, 地平线雾 0–1, 时间)
//
// 大气常数和 atmosphere.rs 是同一组：CPU 那边烘的环境光和这里画的天要对得上。

const SKY_EARTH: f32 = 6360000.0;
const SKY_TOP: f32 = 6420000.0;
const SKY_HR: f32 = 8000.0;
const SKY_HM: f32 = 1200.0;
const SKY_BETA_R: vec3<f32> = vec3<f32>(5.8e-6, 13.5e-6, 33.1e-6);
const SKY_BETA_M: f32 = 21e-6;
const SKY_G: f32 = 0.76;
const SKY_PI: f32 = 3.14159265;
const CLOUD_BOTTOM: f32 = 1500.0;
const CLOUD_TOP: f32 = 4200.0;

fn sky_exit(origin: vec3<f32>, dir: vec3<f32>, radius: f32) -> f32 {
    let b = dot(origin, dir);
    let c = dot(origin, origin) - radius * radius;
    let disc = b * b - c;
    if (disc < 0.0) {
        return 0.0;
    }
    return -b + sqrt(disc);
}

// 朝太阳走到大气顶的光学深度 (瑞利, 米氏)；撞地球时 x = -1。
fn sky_light_depth(origin: vec3<f32>, sun: vec3<f32>) -> vec2<f32> {
    let b = dot(origin, sun);
    let c = dot(origin, origin) - SKY_EARTH * SKY_EARTH;
    if (b < 0.0 && b * b - c > 0.0) {
        return vec2<f32>(-1.0, 0.0);
    }
    let path = sky_exit(origin, sun, SKY_TOP);
    let stride = path / 6.0;
    var depth = vec2<f32>(0.0);
    for (var i = 0; i < 6; i = i + 1) {
        let p = origin + sun * (stride * (f32(i) + 0.5));
        let h = max(length(p) - SKY_EARTH, 0.0);
        depth += vec2<f32>(exp(-h / SKY_HR), exp(-h / SKY_HM)) * stride;
    }
    return depth;
}

fn sky_extinction(depth_r: f32, depth_m: f32, rayleigh: f32, mie: f32) -> vec3<f32> {
    let tau = SKY_BETA_R * rayleigh * depth_r + vec3<f32>(SKY_BETA_M * mie * 1.1 * depth_m);
    return exp(-tau);
}

// 大气辐射（不含日盘）。和 CPU 版 `radiance` 同一算法。
fn atmosphere_radiance(dir_in: vec3<f32>, sun: vec3<f32>, altitude: f32, rayleigh: f32, mie: f32, intensity: f32) -> vec3<f32> {
    let dir = normalize(vec3<f32>(dir_in.x, max(dir_in.y, 0.0), dir_in.z) + vec3<f32>(0.0, 1e-5, 0.0));
    let origin = vec3<f32>(0.0, SKY_EARTH + max(altitude, 1.0), 0.0);
    // 视线最长积 80 公里：多次散射的替身，地平线才发白而不是发黄（见 atmosphere.rs）。
    let path = min(sky_exit(origin, dir, SKY_TOP), 80000.0);
    let stride = path / 16.0;
    let mu = dot(dir, sun);
    let phase_r = 3.0 / (16.0 * SKY_PI) * (1.0 + mu * mu);
    let g = SKY_G;
    let phase_m = 3.0 / (8.0 * SKY_PI) * ((1.0 - g * g) * (1.0 + mu * mu))
        / ((2.0 + g * g) * pow(1.0 + g * g - 2.0 * g * mu, 1.5));
    var sum_r = vec3<f32>(0.0);
    var sum_m = vec3<f32>(0.0);
    var depth_r = 0.0;
    var depth_m = 0.0;
    for (var i = 0; i < 16; i = i + 1) {
        let p = origin + dir * (stride * (f32(i) + 0.5));
        let h = max(length(p) - SKY_EARTH, 0.0);
        let hr = exp(-h / SKY_HR) * stride;
        let hm = exp(-h / SKY_HM) * stride;
        depth_r += hr;
        depth_m += hm;
        let light = sky_light_depth(p, sun);
        if (light.x >= 0.0) {
            let attenuation = sky_extinction(depth_r + light.x, depth_m + light.y, rayleigh, mie);
            sum_r += attenuation * hr;
            sum_m += attenuation * hm;
        }
    }
    return (sum_r * SKY_BETA_R * rayleigh * phase_r + sum_m * (SKY_BETA_M * mie) * phase_m) * intensity;
}

fn sky_sun_transmittance(sun: vec3<f32>, altitude: f32, rayleigh: f32, mie: f32) -> vec3<f32> {
    let origin = vec3<f32>(0.0, SKY_EARTH + max(altitude, 1.0), 0.0);
    let depth = sky_light_depth(origin, sun);
    if (depth.x < 0.0) {
        return vec3<f32>(0.0);
    }
    return sky_extinction(depth.x, depth.y, rayleigh, mie);
}

// ── 云：三维值噪声的分形和 ──
fn sky_hash3(p: vec3<f32>) -> f32 {
    let q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    let r = q + dot(q, q.yxz + 33.33);
    return fract((r.x + r.y) * r.z);
}

fn sky_noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let n000 = sky_hash3(i);
    let n100 = sky_hash3(i + vec3<f32>(1.0, 0.0, 0.0));
    let n010 = sky_hash3(i + vec3<f32>(0.0, 1.0, 0.0));
    let n110 = sky_hash3(i + vec3<f32>(1.0, 1.0, 0.0));
    let n001 = sky_hash3(i + vec3<f32>(0.0, 0.0, 1.0));
    let n101 = sky_hash3(i + vec3<f32>(1.0, 0.0, 1.0));
    let n011 = sky_hash3(i + vec3<f32>(0.0, 1.0, 1.0));
    let n111 = sky_hash3(i + vec3<f32>(1.0, 1.0, 1.0));
    return mix(
        mix(mix(n000, n100, u.x), mix(n010, n110, u.x), u.y),
        mix(mix(n001, n101, u.x), mix(n011, n111, u.x), u.y),
        u.z,
    );
}

fn sky_fbm(p: vec3<f32>) -> f32 {
    var value = 0.0;
    var amplitude = 0.55;
    var q = p;
    for (var i = 0; i < 4; i = i + 1) {
        value += sky_noise3(q) * amplitude;
        q = q * 2.07 + vec3<f32>(17.1, 3.3, 9.7);
        amplitude *= 0.48;
    }
    return value;
}

// offset = (风吹的偏移 x, 形状演化, 偏移 z)：云不光整体飘走，自己也在慢慢变形。
fn cloud_density(p: vec3<f32>, coverage: f32, offset: vec3<f32>) -> f32 {
    let h = clamp((p.y - CLOUD_BOTTOM) / (CLOUD_TOP - CLOUD_BOTTOM), 0.0, 1.0);
    // 积云的形状：底平、顶圆。
    let profile = smoothstep(0.0, 0.12, h) * (1.0 - smoothstep(0.35, 1.0, h));
    let q = (p + vec3<f32>(offset.x, 0.0, offset.z)) * vec3<f32>(0.00032, 0.0006, 0.00032);
    let shape = sky_fbm(q + vec3<f32>(0.0, offset.y, 0.0));
    // 低频的大块决定哪儿有云团，覆盖率越高阈值越低。
    let mass = sky_noise3(q * 0.23 + vec3<f32>(3.1, 0.0, 7.7));
    // 覆盖率 → 阈值：0.37 时只有噪声的峰顶成云（一朵朵积云），1 时几乎铺满。
    let threshold = 1.02 - coverage * 0.72;
    let d = (shape * 0.7 + mass * 0.5) * profile - threshold;
    return max(d, 0.0) * 2.4;
}

// 地平线雾带：取这一处天色的亮度，往偏灰的蓝去；越贴近地平线越浓。
fn sky_horizon_haze(color: vec3<f32>, dir: vec3<f32>, haze: f32) -> vec3<f32> {
    // 不提亮、只去掉一部分饱和度：海平线是灰蓝，不是白的。
    let lum = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let grey = mix(color, vec3<f32>(lum) * vec3<f32>(0.85, 0.93, 1.05), 0.6) * 0.8;
    let band = exp(-max(dir.y, 0.0) * 22.0) * clamp(haze * 1.4, 0.0, 0.6);
    return mix(color, grey, band);
}

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    let p = surface.params;
    let sun = normalize(p[0].xyz);
    let coverage = p[0].w;
    let rayleigh = p[1].x;
    let mie = p[1].y;
    let intensity = p[1].z;
    let density = p[1].w;
    let offset = vec3<f32>(p[2].x, p[3].w * 0.004, p[2].y);
    let steps = i32(p[2].z);
    let night_light = p[2].w;
    let altitude = p[3].x;
    let disc_radius = p[3].y;
    let star_brightness = 1.5;
    let haze = p[3].z;

    let dir = normalize(surface.world_position - globals.camera_position.xyz);
    var color = atmosphere_radiance(dir, sun, altitude, rayleigh, mie, intensity);
    let sun_t = sky_sun_transmittance(sun, altitude, rayleigh, mie);

    // ── 夜空：一点底色 + 星星 ──
    let night = smoothstep(0.08, -0.12, sun.y);
    if (night > 0.0 && dir.y > 0.0) {
        color += vec3<f32>(0.004, 0.006, 0.012) * night_light;
        let cell = floor(dir * 420.0);
        let h = sky_hash3(cell);
        let star = step(0.9975, h) * pow(fract(h * 977.0), 3.0);
        let twinkle = 0.7 + 0.3 * sin(p[3].w * 3.0 + h * 100.0);
        color += vec3<f32>(star * twinkle * star_brightness * night * smoothstep(0.0, 0.15, dir.y));
    }

    // ── 日盘：边缘暗一点（临边昏暗） ──
    let cos_disc = cos(radians(disc_radius));
    let mu = dot(dir, sun);
    if (mu > cos_disc && dir.y > -0.01) {
        let r = sqrt(max(1.0 - (mu - cos_disc) / (1.0 - cos_disc), 0.0));
        let limb = 1.0 - 0.6 * (1.0 - sqrt(max(1.0 - r * r, 0.0)));
        color += sun_t * intensity * 60.0 * limb;
    }

    // ── 体积云 ──
    if (coverage > 0.01 && dir.y > 0.015) {
        let origin_y = altitude;
        let t0 = (CLOUD_BOTTOM - origin_y) / dir.y;
        let t1 = min((CLOUD_TOP - origin_y) / dir.y, t0 + 14000.0);
        let march = min(t1 - t0, 30000.0);
        let count = max(steps, 4);
        let stride = march / f32(count);
        // 每个像素错开一点起点，步数少时的分层条纹变成细噪点。
        let jitter = sky_hash3(vec3<f32>(surface.screen_uv * 1024.0, p[3].w));
        var transmittance = 1.0;
        var scattered = vec3<f32>(0.0);
        let sun_light = sun_t * intensity * 0.9 + vec3<f32>(0.02, 0.025, 0.04) * night_light;
        let ambient = atmosphere_radiance(vec3<f32>(0.0, 1.0, 0.0), sun, altitude, rayleigh, mie, intensity) * 1.4
            + atmosphere_radiance(vec3<f32>(dir.x, 0.05, dir.z), sun, altitude, rayleigh, mie, intensity) * 0.6;
        // 相位：前向散射（逆光看云边是亮的）和一点后向散射混合。
        let g1 = 0.6;
        let hg = (1.0 - g1 * g1) / pow(1.0 + g1 * g1 - 2.0 * g1 * mu, 1.5) / (4.0 * SKY_PI);
        let phase = mix(0.08, hg, 0.7);
        for (var i = 0; i < count; i = i + 1) {
            let t = t0 + stride * (f32(i) + jitter);
            let pos = vec3<f32>(dir.x * t, origin_y + dir.y * t, dir.z * t);
            let d = cloud_density(pos, coverage, offset) * density;
            if (d > 0.001) {
                // 朝太阳走两步估自阴影。
                var shadow = 0.0;
                for (var j = 1; j <= 2; j = j + 1) {
                    shadow += cloud_density(pos + sun * (f32(j) * 260.0), coverage, offset) * density * 260.0;
                }
                let light_t = exp(-shadow * 0.0035);
                // 「糖粉」效应：云团里面朝太阳的那面反而暗一点，边缘更立体。
                let powder = 1.0 - exp(-d * stride * 0.006);
                let sigma = d * 0.0035;
                let luminance = sun_light * light_t * phase * 4.0 * mix(1.0, powder, 0.5) + ambient * 0.12;
                let absorbed = 1.0 - exp(-sigma * stride);
                scattered += transmittance * luminance * absorbed;
                transmittance *= exp(-sigma * stride);
                if (transmittance < 0.02) {
                    break;
                }
            }
        }
        // 远处的云融进大气。
        let aerial = exp(-t0 * 0.00004);
        color = color * mix(1.0, transmittance, aerial) + scattered * aerial;
    }

    // 地平线雾：贴着海平线一道灰蓝的带子——远处水汽把天和海的分界抹淡。
    color = sky_horizon_haze(color, dir, haze);

    // 地平线以下：给一个连续的颜色（海面会盖住它，但边缘处要接得上）。
    if (dir.y < 0.0) {
        color = color * (1.0 + dir.y * 0.6);
    }

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
