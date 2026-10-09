//! 海浪能量谱：JONSWAP 频率谱 × 方向分布。
//!
//! JONSWAP 是北海实测拟合出来的「有限风区」风浪谱：在 Pierson–Moskowitz 谱上乘了一个
//! 峰值增强因子 γ，峰更尖——真实海面的能量比 PM 谱更集中在峰值波长附近。
//! 方向分布用 Mitsuyasu 的 cos²ˢ 模型：峰值频率附近的波最「齐」，偏离越远越散。
//!
//! 涌浪（远处风暴传过来的长周期波）另加一份窄谱：峰值频率低、方向集中、有自己的方向。

use std::f32::consts::PI;

/// 重力加速度。
pub const GRAVITY: f32 = 9.81;

/// 谱的参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectrumParams {
    /// 10 米高处的风速（m/s）。
    pub wind_speed: f32,
    /// 风向（弧度，0 = +X，逆时针朝 +Z）。
    pub wind_direction: f32,
    /// 风区长度（米）：风吹过多远的水面。越长浪越大、越成熟。
    pub fetch: f32,
    /// 峰值波长（米）。`None` 时由风速和风区按 JONSWAP 经验公式算。
    pub peak_wavelength: Option<f32>,
    /// 峰值增强因子，标准值 3.3。1 就退化成 Pierson–Moskowitz 谱。
    pub peak_enhancement: f32,
    /// 涌浪强度（0 = 没有）。
    pub swell: f32,
    /// 涌浪方向（弧度）。
    pub swell_direction: f32,
    /// 涌浪的峰值波长（米）。
    pub swell_wavelength: f32,
    /// 短波压制长度（米）：比它短得多的波按 `exp(-k²l²)` 衰减，免得网格采样不了的涟漪闪烁。
    pub short_wave_cutoff: f32,
    /// 能量倍数（1 = 按谱算的真实海况）。0.5 大约是波高打七折——场景要的是「好看的海」
    /// 而不是「15 m/s 风吹了三百公里的真实海况」时调它，比改风速更不影响波形。
    pub energy: f32,
}

impl Default for SpectrumParams {
    fn default() -> Self {
        Self {
            wind_speed: 10.0,
            wind_direction: 0.3,
            fetch: 300_000.0,
            peak_wavelength: None,
            peak_enhancement: 3.3,
            swell: 0.3,
            swell_direction: 1.1,
            swell_wavelength: 180.0,
            short_wave_cutoff: 0.02,
            energy: 1.0,
        }
    }
}

/// 深水色散关系：`ω = √(g k)`。
pub fn dispersion(k: f32) -> f32 {
    (GRAVITY * k).sqrt()
}

/// JONSWAP 频率谱 `S(ω)`（m²·s）。
pub fn jonswap(omega: f32, wind_speed: f32, fetch: f32, peak_omega: f32, gamma: f32) -> f32 {
    if omega <= 1e-4 {
        return 0.0;
    }
    let u = wind_speed.max(0.1);
    // 无量纲风区决定 Phillips 常数 α：风区越短，谱越「年轻」、α 越大。
    let alpha = 0.076 * (u * u / (fetch * GRAVITY)).powf(0.22);
    let sigma = if omega <= peak_omega { 0.07 } else { 0.09 };
    let r = (-(omega - peak_omega).powi(2) / (2.0 * sigma * sigma * peak_omega * peak_omega)).exp();
    let pm =
        alpha * GRAVITY * GRAVITY / omega.powi(5) * (-1.25 * (peak_omega / omega).powi(4)).exp();
    pm * gamma.powf(r)
}

/// 由风速和风区估峰值角频率（JONSWAP 经验公式）。
pub fn peak_omega_from_wind(wind_speed: f32, fetch: f32) -> f32 {
    let u = wind_speed.max(0.1);
    22.0 * (GRAVITY * GRAVITY / (u * fetch)).powf(1.0 / 3.0)
}

/// 由峰值波长求峰值角频率。
pub fn peak_omega_from_wavelength(wavelength: f32) -> f32 {
    dispersion(2.0 * PI / wavelength.max(0.1))
}

/// `ln Γ(x)`，Lanczos 近似（x > 0）。
fn ln_gamma(x: f32) -> f32 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    let x = f64::from(x) - 1.0;
    let mut sum = C[0];
    for (i, c) in C.iter().enumerate().skip(1) {
        sum += c / (x + i as f64);
    }
    let t = x + G + 0.5;
    (0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()) as f32
}

/// cos²ˢ 方向分布，`∫ D dθ = 1`（θ 在 [-π, π]）。
pub fn directional(theta: f32, s: f32) -> f32 {
    let s = s.clamp(0.1, 60.0);
    // 归一化常数 Γ(s+1) / (2√π Γ(s+½))。
    let norm = (ln_gamma(s + 1.0) - ln_gamma(s + 0.5)).exp() / (2.0 * PI.sqrt());
    norm * (theta * 0.5).cos().abs().powf(2.0 * s)
}

/// Mitsuyasu 的扩展参数：峰值附近 s 最大（波最齐），离峰越远越散。
fn spreading_exponent(omega: f32, peak_omega: f32, wind_speed: f32) -> f32 {
    let s_peak = 11.5 * (peak_omega * wind_speed.max(0.1) / GRAVITY).powf(-2.5);
    let ratio = omega / peak_omega;
    if ratio <= 1.0 {
        s_peak * ratio.powi(5)
    } else {
        s_peak * ratio.powf(-2.5)
    }
}

/// 把角度差折回 [-π, π]。
fn wrap(angle: f32) -> f32 {
    (angle + PI).rem_euclid(2.0 * PI) - PI
}

/// 二维波数谱 `S(kx, kz)`（m⁴）：频率谱 × 方向分布 × 雅可比 `dω/dk / k`。
pub fn spectrum(params: &SpectrumParams, kx: f32, kz: f32) -> f32 {
    let k = (kx * kx + kz * kz).sqrt();
    if k < 1e-5 {
        return 0.0;
    }
    let omega = dispersion(k);
    let theta = kz.atan2(kx);
    // dω/dk = g / (2ω)，再除以 k 从极坐标换到直角坐标。
    let jacobian = GRAVITY / (2.0 * omega) / k;

    let peak = params.peak_wavelength.map_or_else(
        || peak_omega_from_wind(params.wind_speed, params.fetch),
        peak_omega_from_wavelength,
    );
    let s = spreading_exponent(omega, peak, params.wind_speed);
    let wind_sea = jonswap(
        omega,
        params.wind_speed,
        params.fetch,
        peak,
        params.peak_enhancement,
    ) * directional(wrap(theta - params.wind_direction), s);

    // 涌浪：峰更尖（γ 大）、方向集中（s 大），能量按强度缩放。
    let swell = if params.swell > 0.0 {
        let swell_peak = peak_omega_from_wavelength(params.swell_wavelength);
        params.swell
            * jonswap(
                omega,
                params.wind_speed.max(6.0),
                params.fetch * 4.0,
                swell_peak,
                6.0,
            )
            * directional(wrap(theta - params.swell_direction), 24.0)
    } else {
        0.0
    };

    let damping = (-(k * params.short_wave_cutoff).powi(2)).exp();
    (wind_sea + swell) * jacobian * damping * params.energy.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_spreading_integrates_to_one() {
        for s in [0.5, 2.0, 8.0, 24.0] {
            let steps = 4000;
            let sum: f32 = (0..steps)
                .map(|i| directional(-PI + (i as f32 + 0.5) / steps as f32 * 2.0 * PI, s))
                .sum::<f32>()
                * (2.0 * PI / steps as f32);
            assert!((sum - 1.0).abs() < 0.02, "s = {s}: ∫D = {sum}");
        }
    }

    #[test]
    fn jonswap_peaks_at_the_peak_frequency() {
        let peak = peak_omega_from_wavelength(60.0);
        let at = |w: f32| jonswap(w, 12.0, 100_000.0, peak, 3.3);
        assert!(at(peak) > at(peak * 0.8) && at(peak) > at(peak * 1.25));
    }

    #[test]
    fn stronger_wind_means_more_energy() {
        let total = |u: f32| {
            let params = SpectrumParams {
                wind_speed: u,
                swell: 0.0,
                ..Default::default()
            };
            let mut sum = 0.0;
            for i in 1..200 {
                for j in -100..100 {
                    sum += spectrum(&params, i as f32 * 0.01, j as f32 * 0.01);
                }
            }
            sum
        };
        assert!(total(15.0) > total(8.0) * 2.0);
    }
}
