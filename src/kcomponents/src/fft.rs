//! 二维复数 FFT（基 2，原地迭代）。海洋模拟每帧要做十几次 128² / 256² 的逆变换，
//! 旋转因子和位反转表在 [`Fft2d::new`] 里一次算好。

use std::ops::{Add, Mul, Sub};

/// 单精度复数。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    pub const ZERO: Self = Self { re: 0.0, im: 0.0 };

    pub const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    /// `e^{iθ}`。
    pub fn from_angle(theta: f32) -> Self {
        let (s, c) = theta.sin_cos();
        Self { re: c, im: s }
    }

    pub fn conj(self) -> Self {
        Self {
            re: self.re,
            im: -self.im,
        }
    }

    /// 乘以 `i`。
    pub fn mul_i(self) -> Self {
        Self {
            re: -self.im,
            im: self.re,
        }
    }

    pub fn scale(self, s: f32) -> Self {
        Self {
            re: self.re * s,
            im: self.im * s,
        }
    }
}

impl Add for Complex {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}

impl Sub for Complex {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
}

impl Mul for Complex {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}

/// `n × n` 的二维 FFT。
#[derive(Debug, Clone)]
pub struct Fft2d {
    n: usize,
    /// 逆变换的旋转因子 `e^{+2πik/n}`，`k < n/2`。
    twiddles: Vec<Complex>,
    /// 位反转后的下标。
    reversed: Vec<usize>,
    /// 按列变换时用的转置缓冲。
    scratch: Vec<Complex>,
}

impl Fft2d {
    /// `n` 必须是 2 的幂。
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "FFT 尺寸要是 2 的幂：{n}");
        let bits = n.trailing_zeros();
        let reversed = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let twiddles = (0..n / 2)
            .map(|k| Complex::from_angle(2.0 * std::f32::consts::PI * k as f32 / n as f32))
            .collect();
        Self {
            n,
            twiddles,
            reversed,
            scratch: vec![Complex::ZERO; n * n],
        }
    }

    pub fn size(&self) -> usize {
        self.n
    }

    /// 一维**逆**变换（不除以 n）：`x[j] = Σ X[k] e^{+2πijk/n}`。
    fn inverse_1d(&self, data: &mut [Complex]) {
        let n = self.n;
        for i in 0..n {
            let j = self.reversed[i];
            if i < j {
                data.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let w = self.twiddles[k * step];
                    let a = data[start + k];
                    let b = data[start + k + half] * w;
                    data[start + k] = a + b;
                    data[start + k + half] = a - b;
                }
            }
            len *= 2;
        }
    }

    /// 二维逆变换，原地。行主序 `data[z * n + x]`。不归一化——海洋频谱的振幅已经按这个约定算好了。
    pub fn inverse(&mut self, data: &mut [Complex]) {
        let n = self.n;
        assert_eq!(data.len(), n * n);
        for row in data.chunks_exact_mut(n) {
            self.inverse_1d(row);
        }
        // 列变换：转置 → 按行变换 → 转置回来。比跨步访问对缓存友好得多。
        let mut scratch = std::mem::take(&mut self.scratch);
        transpose(data, &mut scratch, n);
        for row in scratch.chunks_exact_mut(n) {
            self.inverse_1d(row);
        }
        transpose(&scratch, data, n);
        self.scratch = scratch;
    }
}

fn transpose(from: &[Complex], to: &mut [Complex], n: usize) {
    // 分块转置，块内连续访问。
    const BLOCK: usize = 16;
    for bz in (0..n).step_by(BLOCK) {
        for bx in (0..n).step_by(BLOCK) {
            for z in bz..(bz + BLOCK).min(n) {
                for x in bx..(bx + BLOCK).min(n) {
                    to[x * n + z] = from[z * n + x];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 直接按定义算的二维逆 DFT，拿来对照。
    fn naive(data: &[Complex], n: usize) -> Vec<Complex> {
        let mut out = vec![Complex::ZERO; n * n];
        for z in 0..n {
            for x in 0..n {
                let mut sum = Complex::ZERO;
                for kz in 0..n {
                    for kx in 0..n {
                        let angle =
                            2.0 * std::f32::consts::PI * ((kx * x + kz * z) as f32) / n as f32;
                        sum = sum + data[kz * n + kx] * Complex::from_angle(angle);
                    }
                }
                out[z * n + x] = sum;
            }
        }
        out
    }

    #[test]
    fn matches_the_definition() {
        let n = 8;
        let mut rng = kmath::Rng::new(3);
        let input: Vec<Complex> = (0..n * n)
            .map(|_| Complex::new(rng.next_signed(), rng.next_signed()))
            .collect();
        let expected = naive(&input, n);
        let mut fft = Fft2d::new(n);
        let mut data = input.clone();
        fft.inverse(&mut data);
        for (a, b) in data.iter().zip(&expected) {
            assert!(
                (a.re - b.re).abs() < 1e-3 && (a.im - b.im).abs() < 1e-3,
                "{a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn a_single_frequency_becomes_a_plane_wave() {
        let n = 16;
        let mut data = vec![Complex::ZERO; n * n];
        data[2] = Complex::new(1.0, 0.0); // kx = 2, kz = 0
        Fft2d::new(n).inverse(&mut data);
        for x in 0..n {
            let expected = (2.0 * std::f32::consts::PI * 2.0 * x as f32 / n as f32).cos();
            assert!((data[5 * n + x].re - expected).abs() < 1e-4);
        }
    }
}
