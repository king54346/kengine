//! 色调映射曲线。
//!
//! PBR 的输出是 HDR，高光轻易超过 1，必须压回 `[0, 1]` 才能显示。
//! 这里的 CPU 实现与 `post.wgsl` 中的 WGSL 版本一一对应——
//! 曲线的数学性质（单调、过原点、不越界）在这里断言，着色器没法测。

/// 色调映射算子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToneMapping {
    /// 直接钳制。保留原始色彩但高光会硬切，出现死白块。
    Clamp,
    /// Reinhard：`c / (1 + c)`。简单柔和，但整体会偏灰。
    Reinhard,
    /// ACES 近似（Narkowicz 拟合）。对比度更高、高光滚降更自然，是通用默认值。
    #[default]
    Aces,
}

impl ToneMapping {
    /// 该算子在 WGSL 中对应的分支编号。
    pub fn index(&self) -> u32 {
        match self {
            Self::Clamp => 0,
            Self::Reinhard => 1,
            Self::Aces => 2,
        }
    }

    /// 对单个通道求值。
    pub fn apply(&self, value: f32) -> f32 {
        let value = value.max(0.0);
        match self {
            Self::Clamp => value.min(1.0),
            Self::Reinhard => value / (1.0 + value),
            Self::Aces => aces(value),
        }
    }
}

/// ACES filmic 的 Narkowicz 拟合。
fn aces(x: f32) -> f32 {
    // 先夹住输入：x² 在 f32 上限附近会溢出成 inf，inf/inf 得到 NaN。
    // 65504 是半精度的最大值，HDR 目标本来也存不下更大的数。
    let x = x.min(65504.0);

    const A: f32 = 2.51;
    const B: f32 = 0.03;
    const C: f32 = 2.43;
    const D: f32 = 0.59;
    const E: f32 = 0.14;

    ((x * (A * x + B)) / (x * (C * x + D) + E)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod test {
    use super::*;

    const OPERATORS: [ToneMapping; 3] =
        [ToneMapping::Clamp, ToneMapping::Reinhard, ToneMapping::Aces];

    #[test]
    fn black_maps_to_black() {
        // 曲线必须过原点，否则暗部会整体抬升，画面发灰。
        for operator in OPERATORS {
            assert_eq!(operator.apply(0.0), 0.0, "{operator:?} 未过原点");
        }
    }

    #[test]
    fn output_never_exceeds_one() {
        for operator in OPERATORS {
            for i in 0..2000 {
                let input = i as f32 * 0.5;
                let output = operator.apply(input);

                assert!(
                    (0.0..=1.0).contains(&output),
                    "{operator:?} 在输入 {input} 时输出 {output} 越界"
                );
            }
        }
    }

    #[test]
    fn curves_are_monotonic() {
        // 单调性保证亮的地方映射后依然更亮，否则会出现亮度反转。
        for operator in OPERATORS {
            let mut previous = operator.apply(0.0);

            for i in 1..=1000 {
                let current = operator.apply(i as f32 * 0.02);
                assert!(
                    current >= previous - 1e-6,
                    "{operator:?} 在第 {i} 步出现亮度反转"
                );
                previous = current;
            }
        }
    }

    #[test]
    fn negative_input_is_clamped_to_zero() {
        // 光照计算偶尔会产生极小的负值，不能让它变成 NaN 或诡异的亮点。
        for operator in OPERATORS {
            assert_eq!(operator.apply(-5.0), 0.0);
        }
    }

    #[test]
    fn output_is_always_finite() {
        for operator in OPERATORS {
            for input in [0.0, 1e-8, 1.0, 1e4, f32::MAX] {
                assert!(
                    operator.apply(input).is_finite(),
                    "{operator:?} 在输入 {input} 时产生非有限值"
                );
            }
        }
    }

    #[test]
    fn bright_values_saturate_towards_one() {
        // 极亮输入应当逼近但不超过 1。
        for operator in OPERATORS {
            assert!(operator.apply(1000.0) > 0.95, "{operator:?} 高光未能提亮");
        }
    }

    #[test]
    fn aces_preserves_more_brightness_than_reinhard() {
        // Reinhard 把一切都往下压，中间调发灰；ACES 的 S 曲线保留更多亮度，
        // 高光滚降也更平缓。这是选它作默认算子的实际理由。
        for input in [0.1, 0.5, 1.0, 2.0] {
            assert!(
                ToneMapping::Aces.apply(input) > ToneMapping::Reinhard.apply(input),
                "输入 {input} 时 ACES 未比 Reinhard 更亮"
            );
        }
    }

    #[test]
    fn reinhard_matches_its_definition() {
        for input in [0.5, 1.0, 4.0] {
            let expected = input / (1.0 + input);
            assert!((ToneMapping::Reinhard.apply(input) - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn wgsl_indices_are_distinct_and_stable() {
        // 这些编号会直接写进 uniform，改动等于改变着色器行为。
        assert_eq!(ToneMapping::Clamp.index(), 0);
        assert_eq!(ToneMapping::Reinhard.index(), 1);
        assert_eq!(ToneMapping::Aces.index(), 2);
    }
}
