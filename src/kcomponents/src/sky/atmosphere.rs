//! 大气单次散射（CPU 版）。和 `sky.wgsl` 里的 `atmosphere_radiance` 是同一个模型、同一组常数：
//! CPU 这边用来烘环境光、算太阳颜色和地平线颜色，GPU 那边画天穹——两边对得上，
//! 水面反射的天和头顶看到的天才是同一片天。
//!
//! 模型：球形大气，瑞利（空气分子，蓝天、红日落）+ 米氏（气溶胶，太阳周围的白晕、雾霾）。
//! 沿视线数值积分，每个采样点再朝太阳积一段算透射率。

use kmath::Vec3;

pub const EARTH_RADIUS: f32 = 6_360_000.0;
pub const ATMOSPHERE_RADIUS: f32 = 6_420_000.0;
/// 瑞利 / 米氏的标高（米）：密度按 e^(-h/H) 衰减。
pub const RAYLEIGH_HEIGHT: f32 = 8_000.0;
pub const MIE_HEIGHT: f32 = 1_200.0;
/// 海平面的瑞利散射系数（1/米），按 680 / 550 / 440 nm。
pub const RAYLEIGH_BETA: Vec3 = Vec3::new(5.8e-6, 13.5e-6, 33.1e-6);
pub const MIE_BETA: f32 = 21e-6;
/// 米氏相位函数的不对称因子：越接近 1 光越往前散（太阳周围那圈越亮）。
pub const MIE_G: f32 = 0.76;
/// 视线积分的最长距离（米），见 [`radiance`]。
pub const MAX_VIEW_PATH: f32 = 80_000.0;

/// 大气参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtmosphereParams {
    /// 指向太阳的单位向量。
    pub sun_direction: Vec3,
    /// 太阳亮度。
    pub sun_intensity: f32,
    /// 瑞利散射倍数（1 = 地球）。
    pub rayleigh: f32,
    /// 米氏散射倍数（浑浊度，越大越雾蒙蒙）。
    pub mie: f32,
}

fn ray_sphere_exit(origin: Vec3, direction: Vec3, radius: f32) -> f32 {
    let b = origin.dot(direction);
    let c = origin.dot(origin) - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 { 0.0 } else { -b + disc.sqrt() }
}

/// 沿 `direction` 从 `origin` 走到大气顶的光学深度 (瑞利, 米氏)。碰到地面返回 `None`。
fn optical_depth_to_space(origin: Vec3, direction: Vec3, steps: u32) -> Option<(f32, f32)> {
    // 朝下且会撞地球：这一点照不到太阳。
    let b = origin.dot(direction);
    let c = origin.dot(origin) - EARTH_RADIUS * EARTH_RADIUS;
    if b < 0.0 && b * b - c > 0.0 {
        return None;
    }
    let length = ray_sphere_exit(origin, direction, ATMOSPHERE_RADIUS);
    let step = length / steps as f32;
    let (mut rayleigh, mut mie) = (0.0, 0.0);
    for i in 0..steps {
        let p = origin + direction * (step * (i as f32 + 0.5));
        let height = (p.length() - EARTH_RADIUS).max(0.0);
        rayleigh += (-height / RAYLEIGH_HEIGHT).exp() * step;
        mie += (-height / MIE_HEIGHT).exp() * step;
    }
    Some((rayleigh, mie))
}

fn extinction(params: &AtmosphereParams, rayleigh_depth: f32, mie_depth: f32) -> Vec3 {
    let tau = RAYLEIGH_BETA * params.rayleigh * rayleigh_depth
        + Vec3::splat(MIE_BETA * params.mie * 1.1 * mie_depth);
    Vec3::new((-tau.x).exp(), (-tau.y).exp(), (-tau.z).exp())
}

/// 从海平面上方 `altitude` 米往 `direction` 看到的天空辐射（不含日盘）。
pub fn radiance(params: &AtmosphereParams, direction: Vec3, altitude: f32) -> Vec3 {
    const PRIMARY: u32 = 16;
    const LIGHT: u32 = 6;
    let direction = direction.normalize_or(Vec3::Y);
    // 朝下的视线压到地平线：海面以下的「天」由海面自己画，这里只要给个连续的颜色。
    let direction = Vec3::new(direction.x, direction.y.max(0.0), direction.z).normalize_or(Vec3::Y);
    let origin = Vec3::new(0.0, EARTH_RADIUS + altitude.max(1.0), 0.0);
    // 视线最长只积这么远。单次散射模型沿着几乎贴地的视线一路积到大气顶（几百公里），
    // 蓝光在路上全被散射掉，地平线会发黄；真实天空靠多次散射把它补回来，地平线是发白的。
    // 截断是多次散射最便宜的替身。
    let length = ray_sphere_exit(origin, direction, ATMOSPHERE_RADIUS).min(MAX_VIEW_PATH);
    let step = length / PRIMARY as f32;
    let sun = params.sun_direction.normalize_or(Vec3::Y);

    let mu = direction.dot(sun);
    let phase_r = 3.0 / (16.0 * std::f32::consts::PI) * (1.0 + mu * mu);
    let g = MIE_G;
    let phase_m = 3.0 / (8.0 * std::f32::consts::PI) * ((1.0 - g * g) * (1.0 + mu * mu))
        / ((2.0 + g * g) * (1.0 + g * g - 2.0 * g * mu).powf(1.5));

    let (mut sum_r, mut sum_m) = (Vec3::ZERO, Vec3::ZERO);
    let (mut depth_r, mut depth_m) = (0.0, 0.0);
    for i in 0..PRIMARY {
        let p = origin + direction * (step * (i as f32 + 0.5));
        let height = (p.length() - EARTH_RADIUS).max(0.0);
        let hr = (-height / RAYLEIGH_HEIGHT).exp() * step;
        let hm = (-height / MIE_HEIGHT).exp() * step;
        depth_r += hr;
        depth_m += hm;
        if let Some((light_r, light_m)) = optical_depth_to_space(p, sun, LIGHT) {
            let attenuation = extinction(params, depth_r + light_r, depth_m + light_m);
            sum_r += attenuation * hr;
            sum_m += attenuation * hm;
        }
    }
    (sum_r * RAYLEIGH_BETA * params.rayleigh * phase_r + sum_m * (MIE_BETA * params.mie) * phase_m)
        * params.sun_intensity
}

/// 太阳光穿过大气到达 `altitude` 处的透射率（日落时偏红就是这个）。太阳在地平线以下时是 0。
pub fn sun_transmittance(params: &AtmosphereParams, altitude: f32) -> Vec3 {
    let origin = Vec3::new(0.0, EARTH_RADIUS + altitude.max(1.0), 0.0);
    match optical_depth_to_space(origin, params.sun_direction.normalize_or(Vec3::Y), 16) {
        Some((r, m)) => extinction(params, r, m),
        None => Vec3::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(sun: Vec3) -> AtmosphereParams {
        AtmosphereParams {
            sun_direction: sun.normalize(),
            sun_intensity: 20.0,
            rayleigh: 1.0,
            mie: 1.0,
        }
    }

    #[test]
    fn the_noon_sky_is_blue() {
        let sky = radiance(
            &params(Vec3::new(0.2, 1.0, 0.1)),
            Vec3::new(0.0, 0.5, 1.0),
            2.0,
        );
        assert!(sky.z > sky.x * 1.5, "{sky:?}");
    }

    #[test]
    fn the_setting_sun_is_red() {
        let t = sun_transmittance(&params(Vec3::new(1.0, 0.03, 0.0)), 2.0);
        assert!(t.x > t.z * 2.0, "{t:?}");
        let noon = sun_transmittance(&params(Vec3::Y), 2.0);
        assert!(noon.z > 0.6, "{noon:?}");
    }

    #[test]
    fn the_sky_goes_dark_after_sunset() {
        let day = radiance(
            &params(Vec3::new(0.0, 0.5, 1.0)),
            Vec3::new(0.0, 0.3, -1.0),
            2.0,
        );
        let night = radiance(
            &params(Vec3::new(0.0, -0.3, 1.0)),
            Vec3::new(0.0, 0.3, -1.0),
            2.0,
        );
        assert!(night.length() < day.length() * 0.05);
    }
}
