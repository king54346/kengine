//! 画质档位：一处切换，海洋、天空、阴影各自取自己的那几项。

/// 画质。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    Low,
    Medium,
    #[default]
    High,
    Ultra,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Low, Quality::Medium, Quality::High, Quality::Ultra];

    pub fn name(self) -> &'static str {
        match self {
            Quality::Low => "低",
            Quality::Medium => "中",
            Quality::High => "高",
            Quality::Ultra => "极高",
        }
    }

    /// 每个级联的 FFT 尺寸。CPU 上算：256 在 release 下一帧三四毫秒，debug 下慢十倍。
    pub fn fft_size(self) -> usize {
        match self {
            Quality::Low => 64,
            Quality::Medium | Quality::High => 128,
            Quality::Ultra => 256,
        }
    }

    /// 海面网格：`(角向分段, 径向增长率)`。增长率越小圈越密。
    pub fn ocean_grid(self) -> (usize, f32) {
        match self {
            Quality::Low => (128, 0.09),
            Quality::Medium => (192, 0.065),
            Quality::High => (256, 0.05),
            Quality::Ultra => (384, 0.035),
        }
    }

    /// 体积云每条视线走几步。
    pub fn cloud_steps(self) -> u32 {
        match self {
            Quality::Low => 10,
            Quality::Medium => 18,
            Quality::High => 28,
            Quality::Ultra => 48,
        }
    }

    /// 阴影图边长。
    pub fn shadow_resolution(self) -> u32 {
        match self {
            Quality::Low => 1024,
            Quality::Medium => 2048,
            Quality::High => 2048,
            Quality::Ultra => 4096,
        }
    }
}
