//! FFT 海浪模拟（Tessendorf 方法），三个级联。
//!
//! 每个级联是一块边长 `L` 米、`N×N` 采样、首尾无缝平铺的海面。初始频谱 `h₀(k)` 按能量谱
//! 撒高斯噪声生成一次；之后每帧按色散关系转相位得到 `h(k, t)`，逆 FFT 回空间域。
//!
//! 一次模拟出八张图：高度、水平位移 x/z（尖浪，choppy waves）、坡度 x/z（法线）、
//! 雅可比的三个偏导（判断浪尖有没有「翻过来」，生泡沫）。两个实数场打包成一个复数场
//! 一起逆变换，八张图只要四次 FFT。
//!
//! 三个级联各管一段波长（不重叠），合起来从几百米的涌浪到几厘米的涟漪都有，
//! 而且三块的边长互不成整数比，平铺的重复感互相打散。

use super::spectrum::{self, SpectrumParams};
use crate::fft::{Complex, Fft2d};
use std::f32::consts::PI;

/// 一个级联的设置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CascadeSettings {
    /// 平铺块的边长（米）。
    pub length: f32,
    /// 只保留这段波数 `[k_min, k_max)`，和相邻级联拼起来不重叠。
    pub k_min: f32,
    pub k_max: f32,
}

/// 默认的三个级联：涌浪（波长几十到几百米）、风浪（几米到几十米）、涟漪（几厘米到几米）。
pub fn default_cascades() -> [CascadeSettings; 3] {
    let lengths = [640.0_f32, 112.0, 17.0];
    // 相邻两级的分界：下一级平铺块能装下 6 个完整波长的那个波数。
    let boundary = |length: f32| 2.0 * PI / length * 6.0;
    [
        CascadeSettings {
            length: lengths[0],
            k_min: 0.0,
            k_max: boundary(lengths[1]),
        },
        CascadeSettings {
            length: lengths[1],
            k_min: boundary(lengths[1]),
            k_max: boundary(lengths[2]),
        },
        CascadeSettings {
            length: lengths[2],
            k_min: boundary(lengths[2]),
            k_max: f32::INFINITY,
        },
    ]
}

/// 一个级联这一帧的结果（空间域，行主序 `[z * n + x]`）。
#[derive(Debug, Clone, Default)]
pub struct CascadeMaps {
    pub height: Vec<f32>,
    pub dx: Vec<f32>,
    pub dz: Vec<f32>,
    pub slope_x: Vec<f32>,
    pub slope_z: Vec<f32>,
    /// 泡沫 0–1：浪尖翻卷的程度，带时间上的残留。
    pub foam: Vec<f32>,
    /// 高度与水平位移的最大绝对值（编码进纹理时用）。
    pub displacement_range: f32,
    /// 上一步的位移（高度、x、z），求水的速度用：漂浮物要跟着水一起动，
    /// 阻尼得按「相对于水」算，不然浪推不动船、只会拖住它。
    pub prev_height: Vec<f32>,
    pub prev_dx: Vec<f32>,
    pub prev_dz: Vec<f32>,
    /// 上一步到这一步过了多久（秒）。
    pub step: f32,
}

/// 一个级联的模拟状态。
pub struct Cascade {
    pub settings: CascadeSettings,
    n: usize,
    fft: Fft2d,
    /// 初始频谱 `h₀(k)` 与 `conj(h₀(-k))`。
    h0: Vec<Complex>,
    h0_minus_conj: Vec<Complex>,
    omega: Vec<f32>,
    /// `kx/k`、`kz/k`、`kx`、`kz`、`k`（中心化下标）。
    kx: Vec<f32>,
    kz: Vec<f32>,
    k: Vec<f32>,
    buffers: [Vec<Complex>; 4],
    pub maps: CascadeMaps,
}

impl Cascade {
    /// 按谱生成初始频谱。同样的种子出同样的海。
    pub fn new(settings: CascadeSettings, n: usize, params: &SpectrumParams, seed: u64) -> Self {
        let count = n * n;
        let dk = 2.0 * PI / settings.length;
        let mut kx = vec![0.0; count];
        let mut kz = vec![0.0; count];
        let mut k = vec![0.0; count];
        let mut omega = vec![0.0; count];
        for z in 0..n {
            for x in 0..n {
                let i = z * n + x;
                kx[i] = (x as f32 - n as f32 / 2.0) * dk;
                kz[i] = (z as f32 - n as f32 / 2.0) * dk;
                k[i] = (kx[i] * kx[i] + kz[i] * kz[i]).sqrt();
                omega[i] = spectrum::dispersion(k[i]);
            }
        }
        let mut cascade = Self {
            settings,
            n,
            fft: Fft2d::new(n),
            h0: vec![Complex::ZERO; count],
            h0_minus_conj: vec![Complex::ZERO; count],
            omega,
            kx,
            kz,
            k,
            buffers: std::array::from_fn(|_| vec![Complex::ZERO; count]),
            maps: CascadeMaps {
                height: vec![0.0; count],
                dx: vec![0.0; count],
                dz: vec![0.0; count],
                slope_x: vec![0.0; count],
                slope_z: vec![0.0; count],
                foam: vec![0.0; count],
                displacement_range: 0.01,
                prev_height: vec![0.0; count],
                prev_dx: vec![0.0; count],
                prev_dz: vec![0.0; count],
                step: 0.0,
            },
        };
        cascade.respectrum(params, seed);
        cascade
    }

    pub fn size(&self) -> usize {
        self.n
    }

    /// 换谱参数（风速、风向……）后重算初始频谱。随机数用同一个种子，海面「形状」连续。
    pub fn respectrum(&mut self, params: &SpectrumParams, seed: u64) {
        let n = self.n;
        let dk = 2.0 * PI / self.settings.length;
        let mut rng = kmath::Rng::new(seed);
        for i in 0..n * n {
            let (kx, kz, k) = (self.kx[i], self.kz[i], self.k[i]);
            let in_band = k >= self.settings.k_min && k < self.settings.k_max;
            let energy = if in_band {
                spectrum::spectrum(params, kx, kz)
            } else {
                0.0
            };
            // 高斯随机数（Box–Muller）。不管在不在带内都要抽，随机数序列才和参数无关。
            let (g1, g2) = gaussian_pair(&mut rng);
            let amplitude = (2.0 * energy * dk * dk).sqrt() / std::f32::consts::SQRT_2;
            self.h0[i] = Complex::new(g1 * amplitude, g2 * amplitude);
        }
        // conj(h₀(-k))：中心化下标里 -k 对应 (n - x) % n。
        for z in 0..n {
            for x in 0..n {
                let mirrored = ((n - z) % n) * n + (n - x) % n;
                self.h0_minus_conj[z * n + x] = self.h0[mirrored].conj();
            }
        }
    }

    /// 推进到时刻 `time`（秒）。`choppiness` 是水平位移的倍数（0 = 正弦样的圆浪）。
    pub fn update(&mut self, time: f32, dt: f32, choppiness: f32, foam: FoamSettings) {
        let n = self.n;
        let [b0, b1, b2, b3] = &mut self.buffers;
        for i in 0..n * n {
            let phase = Complex::from_angle(self.omega[i] * time);
            let h = self.h0[i] * phase + self.h0_minus_conj[i] * phase.conj();
            let k = self.k[i];
            let (ux, uz) = if k > 1e-6 {
                (self.kx[i] / k, self.kz[i] / k)
            } else {
                (0.0, 0.0)
            };
            let (kx, kz) = (self.kx[i], self.kz[i]);
            // 位移 D = -i k̂ h，坡度 ∂h = i k h，雅可比偏导 ∂D = k̂ k h（按分量）。
            let dx = h.mul_i().scale(-ux);
            let dz = h.mul_i().scale(-uz);
            let sx = h.mul_i().scale(kx);
            let sz = h.mul_i().scale(kz);
            let dxdx = h.scale(ux * kx);
            let dzdz = h.scale(uz * kz);
            let dxdz = h.scale(ux * kz);
            // 两个实数场打包成一个复数场：F = A + iB，逆变换后实部是 a、虚部是 b。
            b0[i] = h + dx.mul_i();
            b1[i] = dz + sx.mul_i();
            b2[i] = sz + dxdx.mul_i();
            b3[i] = dzdz + dxdz.mul_i();
        }
        for buffer in [&mut *b0, &mut *b1, &mut *b2, &mut *b3] {
            self.fft.inverse(buffer);
        }

        let maps = &mut self.maps;
        std::mem::swap(&mut maps.prev_height, &mut maps.height);
        std::mem::swap(&mut maps.prev_dx, &mut maps.dx);
        std::mem::swap(&mut maps.prev_dz, &mut maps.dz);
        maps.step = dt;
        let decay = (-foam.decay * dt).exp();
        let mut range = 0.0f32;
        for z in 0..n {
            for x in 0..n {
                let i = z * n + x;
                // 频谱用的是中心化下标，逆变换结果要乘 (-1)^(x+z) 才是真值。
                let sign = if (x + z) % 2 == 0 { 1.0 } else { -1.0 };
                let height = b0[i].re * sign;
                let dx = b0[i].im * sign * choppiness;
                let dz = b1[i].re * sign * choppiness;
                maps.height[i] = height;
                maps.dx[i] = dx;
                maps.dz[i] = dz;
                maps.slope_x[i] = b1[i].im * sign;
                maps.slope_z[i] = b2[i].re * sign;
                range = range.max(height.abs()).max(dx.abs()).max(dz.abs());

                // 雅可比 J = (1 + λ∂Dx/∂x)(1 + λ∂Dz/∂z) − (λ∂Dx/∂z)²。小于 1 说明水面被挤压，
                // 小于 0 说明翻过来了——那就是破碎的浪尖，生泡沫。
                let jxx = 1.0 + choppiness * b2[i].im * sign;
                let jzz = 1.0 + choppiness * b3[i].re * sign;
                let jxz = choppiness * b3[i].im * sign;
                let jacobian = jxx * jzz - jxz * jxz;
                let fresh =
                    ((foam.threshold - jacobian) * foam.sharpness).clamp(0.0, 1.0) * foam.amount;
                maps.foam[i] = (maps.foam[i] * decay).max(fresh);
            }
        }
        // 编码范围慢慢收、快快放：每帧跟着最大值跳的话，量化台阶会闪。
        maps.displacement_range = (maps.displacement_range * 0.995)
            .max(range * 1.05)
            .max(0.01);
    }

    /// 双线性采样某一张图。`(x, z)` 是世界坐标（米），按平铺块自动环绕。
    pub fn sample(&self, map: &[f32], x: f32, z: f32) -> f32 {
        let n = self.n;
        let scale = n as f32 / self.settings.length;
        let (u, v) = (x * scale, z * scale);
        let (x0, z0) = (u.floor(), v.floor());
        let (fx, fz) = (u - x0, v - z0);
        let wrap = |a: f32| (a as i64).rem_euclid(n as i64) as usize;
        let (x0, z0, x1, z1) = (wrap(x0), wrap(z0), wrap(x0 + 1.0), wrap(z0 + 1.0));
        let at = |x: usize, z: usize| map[z * n + x];
        let top = at(x0, z0) * (1.0 - fx) + at(x1, z0) * fx;
        let bottom = at(x0, z1) * (1.0 - fx) + at(x1, z1) * fx;
        top * (1.0 - fz) + bottom * fz
    }
}

/// 泡沫怎么生、怎么消。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoamSettings {
    /// 雅可比低于它就开始生泡沫。1 左右是「水面被挤压」，越大泡沫越多。
    pub threshold: f32,
    /// 从「刚开始挤」到「全白」的过渡快慢。
    pub sharpness: f32,
    /// 总量倍数。
    pub amount: f32,
    /// 消散速度（1/秒）：泡沫被浪推走之后还留一会儿，形成拖尾。
    pub decay: f32,
}

impl Default for FoamSettings {
    fn default() -> Self {
        // 消散慢一点：浪头过去之后白沫还拖一段，才是一道道的痕迹。
        Self {
            threshold: 0.55,
            sharpness: 2.5,
            amount: 1.0,
            decay: 0.4,
        }
    }
}

fn gaussian_pair(rng: &mut kmath::Rng) -> (f32, f32) {
    let u1 = rng.next_f32().max(1e-7);
    let u2 = rng.next_f32();
    let r = (-2.0 * u1.ln()).sqrt();
    let (s, c) = (2.0 * PI * u2).sin_cos();
    (r * c, r * s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_real_and_moves() {
        let params = SpectrumParams::default();
        let settings = default_cascades()[1];
        let mut cascade = Cascade::new(settings, 64, &params, 7);
        cascade.update(0.0, 0.016, 1.0, FoamSettings::default());
        let before = cascade.maps.height.clone();
        // 有起伏。
        let rms = (before.iter().map(|h| h * h).sum::<f32>() / before.len() as f32).sqrt();
        assert!(rms > 0.01, "rms = {rms}");
        cascade.update(1.0, 0.016, 1.0, FoamSettings::default());
        let moved = before
            .iter()
            .zip(&cascade.maps.height)
            .filter(|(a, b)| (*a - *b).abs() > 1e-3)
            .count();
        assert!(moved > before.len() / 2, "一秒之后大部分点该动了：{moved}");
    }

    #[test]
    fn sampling_wraps_around_the_tile() {
        let params = SpectrumParams::default();
        let settings = default_cascades()[2];
        let mut cascade = Cascade::new(settings, 32, &params, 1);
        cascade.update(3.0, 0.016, 1.0, FoamSettings::default());
        let length = settings.length;
        let a = cascade.sample(&cascade.maps.height, 1.3, 2.7);
        let b = cascade.sample(&cascade.maps.height, 1.3 + length, 2.7 - 2.0 * length);
        assert!((a - b).abs() < 1e-4);
    }

    #[test]
    fn stronger_wind_makes_bigger_waves() {
        // 三个级联加起来比：风大了峰值波长变长，能量会挪到涌浪那一级去，单看一级不准。
        let rms = |wind: f32| {
            let params = SpectrumParams {
                wind_speed: wind,
                swell: 0.0,
                ..Default::default()
            };
            let mut variance = 0.0;
            for settings in default_cascades() {
                let mut cascade = Cascade::new(settings, 64, &params, 3);
                cascade.update(0.0, 0.016, 1.0, FoamSettings::default());
                variance += cascade.maps.height.iter().map(|h| h * h).sum::<f32>() / 4096.0;
            }
            variance.sqrt()
        };
        assert!(rms(16.0) > rms(6.0) * 1.5);
    }
}
