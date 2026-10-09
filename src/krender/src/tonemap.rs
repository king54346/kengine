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
    /// AgX（Blender 4 的默认，three.js 的 `AgXToneMapping`）。
    ///
    /// 和 ACES 最大的区别在**高饱和的高光**：ACES 会把很亮的纯色推向
    /// 更饱和、甚至偏色（亮蓝变紫），AgX 让它们往白色退——火焰、霓虹灯
    /// 看起来更像真的相机拍的。
    Agx,
    /// Khronos PBR Neutral（three.js 的 `NeutralToneMapping`）。
    ///
    /// 专门为「产品图」设计：`[0, 0.8]` 以内几乎是恒等映射，基础色
    /// 是什么颜色显示出来就是什么颜色。只在高光处压缩。
    Neutral,
    /// 优化过的 Cineon 拟合（three.js 的 `CineonToneMapping`）。
    /// 自带 gamma，所以结果偏亮、对比度低，电影胶片的观感。
    Cineon,
}

impl ToneMapping {
    /// 该算子在 WGSL 中对应的分支编号。
    pub fn index(&self) -> u32 {
        match self {
            Self::Clamp => 0,
            Self::Reinhard => 1,
            Self::Aces => 2,
            Self::Agx => 3,
            Self::Neutral => 4,
            Self::Cineon => 5,
        }
    }

    /// 对一个灰阶值求值（三个通道相等时的曲线）。
    pub fn apply(&self, value: f32) -> f32 {
        let value = value.max(0.0);
        match self {
            Self::Clamp => value.min(1.0),
            Self::Reinhard => value / (1.0 + value),
            Self::Aces => aces(value),
            // 这三个不是逐通道的曲线（AgX 和 Neutral 要看三个通道的
            // 关系），这里取灰阶那条。
            Self::Agx => agx([value; 3])[0],
            Self::Neutral => neutral([value; 3])[0],
            Self::Cineon => cineon(value),
        }
    }
}

impl ToneMapping {
    /// 对一个 RGB 颜色求值，和着色器一样：AgX、Neutral 按三个通道一起算，
    /// 其余逐通道。
    pub fn apply_rgb(&self, color: [f32; 3]) -> [f32; 3] {
        let color = color.map(|c| c.max(0.0));
        match self {
            Self::Agx => agx(color),
            Self::Neutral => neutral(color),
            _ => color.map(|c| self.apply(c)),
        }
    }

    /// 反过来：什么样的 HDR 颜色经过这条曲线会变成 `target`。
    ///
    /// 纯色背景要用它：three.js 的 `scene.background = new Color(...)` 是
    /// **清屏色**，不过色调映射，屏幕上就是那个颜色；这边背景和物体在同
    /// 一张 HDR 图里，一起过色调映射。不反算的话，ACES 下 0x111111 会被
    /// 压成纯黑，0xdeebed 会变暗变灰。
    ///
    /// 曲线到不了的值（ACES 永远到不了 1）按能到的最接近值算。先在灰阶
    /// 曲线上逐通道二分，再对整条 RGB 曲线做几轮乘法修正——AgX 和 Neutral
    /// 会让通道互相影响，只用灰阶曲线反算饱和色会偏。
    ///
    /// # 渐近线附近
    ///
    /// Neutral 只在无穷远处才到 1：精确反算白色得到的是上限 256，这么亮的背景
    /// 一进 Bloom 就把整个画面糊成一片（实测：白底 + Neutral 的木纹样板全糊了）。所以精确值要
    /// 比「显示出来是同一个 8 位色阶的最暗那个值」大一倍以上时，改用后者——屏幕上分不出来，
    /// HDR 值从 256 降到 15 左右（和 ACES 下白色的 7 同一个量级）。Reinhard 救不了：它的曲线太平，
    /// 显示成 255 本身就要 221 以上——白底别配 Reinhard。
    pub fn invert(&self, target: [f32; 3]) -> [f32; 3] {
        const LIMIT: f32 = 256.0;
        let ceiling = self.apply(LIMIT);
        let target = target.map(|t| t.clamp(0.0, ceiling));
        let gray_inverse = |t: f32| {
            let (mut lo, mut hi) = (0.0f32, LIMIT);
            for _ in 0..40 {
                let mid = (lo + hi) * 0.5;
                if self.apply(mid) < t {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            (lo + hi) * 0.5
        };
        // 同一个 8 位 sRGB 色阶里最暗的线性值（四舍五入后还落在这一阶）。
        let lowest_same_code = |t: f32| {
            let srgb = if t <= 0.003_130_8 {
                t * 12.92
            } else {
                1.055 * t.powf(1.0 / 2.4) - 0.055
            };
            let edge = ((srgb * 255.0).round() - 0.5).max(0.0) / 255.0;
            if edge <= 0.04045 {
                edge / 12.92
            } else {
                ((edge + 0.055) / 1.055).powf(2.4)
            }
        };
        let target = target.map(|t| {
            let relaxed = lowest_same_code(t);
            if gray_inverse(t) > 2.0 * gray_inverse(relaxed) {
                relaxed
            } else {
                t
            }
        });
        let mut x = target.map(gray_inverse);
        for _ in 0..24 {
            let y = self.apply_rgb(x);
            for c in 0..3 {
                if target[c] <= 1e-6 {
                    x[c] = 0.0;
                } else if y[c] > 1e-6 {
                    x[c] = (x[c] * target[c] / y[c]).clamp(0.0, LIMIT);
                }
            }
        }
        x
    }
}

/// Cineon 的优化拟合，和 three.js 的 `OptimizedCineonToneMapping` 相同。
fn cineon(x: f32) -> f32 {
    let x = (x.min(65504.0) - 0.004).max(0.0);
    ((x * (6.2 * x + 0.5)) / (x * (6.2 * x + 1.7) + 0.06))
        .powf(2.2)
        .clamp(0.0, 1.0)
}

/// 3×3 矩阵乘向量，矩阵按**列**给（和 WGSL 的 `mat3x3` 构造顺序一致）。
fn mul3(columns: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (column, value) in columns.iter().zip(v) {
        for row in 0..3 {
            out[row] += column[row] * value;
        }
    }
    out
}

/// AgX，和 three.js 的实现逐项对应（包括在 Rec.2020 里做）。
fn agx(color: [f32; 3]) -> [f32; 3] {
    const SRGB_TO_REC2020: [[f32; 3]; 3] = [
        [0.6274, 0.0691, 0.0164],
        [0.3293, 0.9195, 0.0880],
        [0.0433, 0.0113, 0.8956],
    ];
    const REC2020_TO_SRGB: [[f32; 3]; 3] = [
        [1.6605, -0.1246, -0.0182],
        [-0.5876, 1.1329, -0.1006],
        [-0.0728, -0.0083, 1.1187],
    ];
    const INSET: [[f32; 3]; 3] = [
        [0.856_627_15, 0.137_318_97, 0.111_898_21],
        [0.095_121_24, 0.761_242, 0.076_799_42],
        [0.048_251_61, 0.101_439_04, 0.811_302_37],
    ];
    const OUTSET: [[f32; 3]; 3] = [
        [1.127_100_6, -0.141_329_76, -0.141_329_76],
        [-0.110_606_64, 1.157_823_7, -0.110_606_64],
        [-0.016_493_94, -0.016_493_94, 1.251_936_4],
    ];
    const MIN_EV: f32 = -12.473_931;
    const MAX_EV: f32 = 4.026_069;

    let color = mul3(INSET, mul3(SRGB_TO_REC2020, color));
    let encoded = color.map(|c| {
        let x = ((c.clamp(1e-10, 65504.0).log2() - MIN_EV) / (MAX_EV - MIN_EV)).clamp(0.0, 1.0);
        let x2 = x * x;
        let x4 = x2 * x2;
        15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x
            - 0.00232
    });
    let color = mul3(OUTSET, encoded).map(|c| c.max(0.0).powf(2.2));
    mul3(REC2020_TO_SRGB, color).map(|c| c.clamp(0.0, 1.0))
}

/// Khronos PBR Neutral。
fn neutral(color: [f32; 3]) -> [f32; 3] {
    const START: f32 = 0.8 - 0.04;
    const DESATURATION: f32 = 0.15;
    let color = color.map(|c| c.clamp(0.0, 65504.0));
    let x = color[0].min(color[1]).min(color[2]);
    let offset = if x < 0.08 { x - 6.25 * x * x } else { 0.04 };
    let color = color.map(|c| c - offset);
    let peak = color[0].max(color[1]).max(color[2]);
    if peak < START {
        return color.map(|c| c.clamp(0.0, 1.0));
    }
    let d = 1.0 - START;
    let new_peak = 1.0 - d * d / (peak + d - START);
    let g = 1.0 - 1.0 / (DESATURATION * (peak - new_peak) + 1.0);
    color.map(|c| {
        let scaled = c * new_peak / peak;
        (scaled + (new_peak - scaled) * g).clamp(0.0, 1.0)
    })
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

    #[test]
    fn invert_round_trips_every_operator() {
        let targets = [
            [0.0056, 0.0056, 0.0056],
            [0.73, 0.83, 0.84],
            [0.5, 0.1, 0.02],
            [0.9, 0.9, 0.9],
        ];
        for mode in [
            ToneMapping::Clamp,
            ToneMapping::Reinhard,
            ToneMapping::Aces,
            ToneMapping::Agx,
            ToneMapping::Neutral,
            ToneMapping::Cineon,
        ] {
            for target in targets {
                let back = mode.apply_rgb(mode.invert(target));
                for c in 0..3 {
                    // AgX 的输入矩阵让极暗的饱和色到不了，放宽一点。
                    let tolerance = if mode == ToneMapping::Agx { 0.02 } else { 2e-3 };
                    assert!(
                        (back[c] - target[c]).abs() < tolerance,
                        "{mode:?} {target:?} → {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_white_background_does_not_invert_to_a_blinding_value() {
        // Reinhard 不在内：c / (1 + c) 太平，显示成 255 要 221 以上，这是曲线本身的事。
        for mode in [ToneMapping::Neutral, ToneMapping::Aces, ToneMapping::Agx] {
            let hdr = mode.invert([1.0; 3]);
            assert!(
                hdr.iter().all(|v| *v < 32.0),
                "{mode:?}：白色反算成了 {hdr:?}"
            );
            // 显示出来还是 255。
            let back = mode.apply_rgb(hdr);
            let srgb = |t: f32| (1.055 * t.powf(1.0 / 2.4) - 0.055) * 255.0;
            assert!(
                back.iter().all(|v| srgb(*v).round() >= 254.0),
                "{mode:?}：{back:?}"
            );
        }
    }

    const OPERATORS: [ToneMapping; 6] = [
        ToneMapping::Clamp,
        ToneMapping::Reinhard,
        ToneMapping::Aces,
        ToneMapping::Agx,
        ToneMapping::Neutral,
        ToneMapping::Cineon,
    ];

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
        assert_eq!(ToneMapping::Agx.index(), 3);
        assert_eq!(ToneMapping::Neutral.index(), 4);
        assert_eq!(ToneMapping::Cineon.index(), 5);
    }

    #[test]
    fn neutral_is_almost_identity_in_the_mid_tones() {
        // 这是它存在的理由：产品图里的基础色显示出来就该是那个颜色。
        for input in [0.2, 0.4, 0.6] {
            assert!((ToneMapping::Neutral.apply(input) - (input - 0.04)).abs() < 0.02);
        }
    }

    #[test]
    fn agx_pushes_saturated_highlights_towards_white() {
        // 非常亮的纯蓝：AgX 该让它往白退，也就是另外两个通道被抬起来。
        let out = agx([0.0, 0.0, 50.0]);
        assert!(
            out[0] > 0.2 && out[1] > 0.2,
            "AgX 没有让高饱和高光退白：{out:?}"
        );
    }
}
