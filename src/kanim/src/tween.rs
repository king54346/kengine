//! 补间：从一个值在给定时间里平滑过渡到另一个值。
//!
//! 和关键帧剪辑（[`AnimationClip`](crate::AnimationClip)）的区别是**起点在运行时才知道**：
//! 「门从现在的位置滑到打开的位置」「血条从当前值降到新值」。
//! 剪辑是美术做好的固定轨迹，补间是代码临时说一句「半秒内过去」。
//!
//! ```
//! use kanim::{Ease, Tween};
//!
//! let mut tween = Tween::new(0.0f32, 10.0, 1.0, Ease::OutCubic);
//! let halfway = tween.advance(0.5);
//! assert!(halfway > 5.0, "先快后慢：一半时间走了一大半");
//! tween.advance(0.5);
//! assert!(tween.finished());
//! assert_eq!(tween.value(), 10.0);
//! ```

use crate::Animatable;
use std::f32::consts::PI;

/// 缓动曲线：把归一化的时间 `t ∈ [0, 1]` 映射成进度。
///
/// 命名照 easings.net / CSS / Godot 的惯例：`In` 是开头慢、`Out` 是结尾慢、
/// `InOut` 两头慢。脚本里用字符串名（`"easeOutCubic"`），见 [`Ease::from_name`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(missing_docs)]
pub enum Ease {
    /// 匀速。
    #[default]
    Linear,
    InQuad,
    OutQuad,
    InOutQuad,
    InCubic,
    OutCubic,
    InOutCubic,
    InQuart,
    OutQuart,
    InOutQuart,
    InSine,
    OutSine,
    InOutSine,
    InExpo,
    OutExpo,
    InOutExpo,
    /// 先往回拉一点再出发。
    InBack,
    /// 冲过头一点再回来。
    OutBack,
    InOutBack,
    /// 像弹簧一样晃几下停住。
    OutElastic,
    /// 像球落地一样弹几下。
    OutBounce,
}

impl Ease {
    /// 所有曲线，按声明顺序。
    pub const ALL: [Ease; 21] = [
        Ease::Linear,
        Ease::InQuad,
        Ease::OutQuad,
        Ease::InOutQuad,
        Ease::InCubic,
        Ease::OutCubic,
        Ease::InOutCubic,
        Ease::InQuart,
        Ease::OutQuart,
        Ease::InOutQuart,
        Ease::InSine,
        Ease::OutSine,
        Ease::InOutSine,
        Ease::InExpo,
        Ease::OutExpo,
        Ease::InOutExpo,
        Ease::InBack,
        Ease::OutBack,
        Ease::InOutBack,
        Ease::OutElastic,
        Ease::OutBounce,
    ];

    /// 按名字找：`"linear"`、`"easeOutCubic"`、`"outCubic"`、`"out_cubic"` 都认，不分大小写。
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let key = key.strip_prefix("ease").unwrap_or(&key);
        Self::ALL
            .into_iter()
            .find(|ease| ease.name().to_ascii_lowercase() == key)
    }

    /// 不带 `ease` 前缀的驼峰名，例如 `"outCubic"`。
    pub fn name(self) -> &'static str {
        match self {
            Ease::Linear => "linear",
            Ease::InQuad => "inQuad",
            Ease::OutQuad => "outQuad",
            Ease::InOutQuad => "inOutQuad",
            Ease::InCubic => "inCubic",
            Ease::OutCubic => "outCubic",
            Ease::InOutCubic => "inOutCubic",
            Ease::InQuart => "inQuart",
            Ease::OutQuart => "outQuart",
            Ease::InOutQuart => "inOutQuart",
            Ease::InSine => "inSine",
            Ease::OutSine => "outSine",
            Ease::InOutSine => "inOutSine",
            Ease::InExpo => "inExpo",
            Ease::OutExpo => "outExpo",
            Ease::InOutExpo => "inOutExpo",
            Ease::InBack => "inBack",
            Ease::OutBack => "outBack",
            Ease::InOutBack => "inOutBack",
            Ease::OutElastic => "outElastic",
            Ease::OutBounce => "outBounce",
        }
    }

    /// 求进度。`t` 先夹到 `[0, 1]`；结果在两端恰好是 0 和 1，中间可能越界（`Back`、`Elastic`）。
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        // 「Out」是「In」倒过来：out(t) = 1 - in(1 - t)。
        let out = |f: fn(f32) -> f32| 1.0 - f(1.0 - t);
        let in_out = |f: fn(f32) -> f32| {
            if t < 0.5 {
                f(2.0 * t) * 0.5
            } else {
                1.0 - f(2.0 - 2.0 * t) * 0.5
            }
        };
        match self {
            Ease::Linear => t,
            Ease::InQuad => quad(t),
            Ease::OutQuad => out(quad),
            Ease::InOutQuad => in_out(quad),
            Ease::InCubic => cubic(t),
            Ease::OutCubic => out(cubic),
            Ease::InOutCubic => in_out(cubic),
            Ease::InQuart => quart(t),
            Ease::OutQuart => out(quart),
            Ease::InOutQuart => in_out(quart),
            Ease::InSine => sine(t),
            Ease::OutSine => out(sine),
            Ease::InOutSine => in_out(sine),
            Ease::InExpo => expo(t),
            Ease::OutExpo => out(expo),
            Ease::InOutExpo => in_out(expo),
            Ease::InBack => back(t),
            Ease::OutBack => out(back),
            Ease::InOutBack => in_out(back),
            Ease::OutElastic => {
                if t == 0.0 || t == 1.0 {
                    t
                } else {
                    2f32.powf(-10.0 * t) * ((t * 10.0 - 0.75) * (2.0 * PI / 3.0)).sin() + 1.0
                }
            }
            Ease::OutBounce => bounce(t),
        }
    }
}

fn quad(t: f32) -> f32 {
    t * t
}
fn cubic(t: f32) -> f32 {
    t * t * t
}
fn quart(t: f32) -> f32 {
    t * t * t * t
}
fn sine(t: f32) -> f32 {
    1.0 - (t * PI / 2.0).cos()
}
fn expo(t: f32) -> f32 {
    if t == 0.0 {
        0.0
    } else {
        2f32.powf(10.0 * t - 10.0)
    }
}
fn back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    (C1 + 1.0) * t * t * t - C1 * t * t
}
fn bounce(t: f32) -> f32 {
    const N: f32 = 7.5625;
    const D: f32 = 2.75;
    if t < 1.0 / D {
        N * t * t
    } else if t < 2.0 / D {
        let t = t - 1.5 / D;
        N * t * t + 0.75
    } else if t < 2.5 / D {
        let t = t - 2.25 / D;
        N * t * t + 0.9375
    } else {
        let t = t - 2.625 / D;
        N * t * t + 0.984375
    }
}

/// 一段补间：`from` 在 `duration` 秒里按 `ease` 过渡到 `to`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween<T: Animatable> {
    /// 起点。
    pub from: T,
    /// 终点。
    pub to: T,
    /// 总时长（秒）。0 或负数表示立刻到终点。
    pub duration: f32,
    /// 缓动曲线。
    pub ease: Ease,
    elapsed: f32,
}

impl<T: Animatable> Tween<T> {
    /// 新建一段补间，还没开始走。
    pub fn new(from: T, to: T, duration: f32, ease: Ease) -> Self {
        Self {
            from,
            to,
            duration,
            ease,
            elapsed: 0.0,
        }
    }

    /// 往前走 `dt` 秒，返回走完之后的值。
    pub fn advance(&mut self, dt: f32) -> T {
        self.elapsed = (self.elapsed + dt.max(0.0)).min(self.duration.max(0.0));
        self.value()
    }

    /// 当前值。
    pub fn value(&self) -> T {
        T::lerp(self.from, self.to, self.ease.apply(self.progress()))
    }

    /// 时间进度 `[0, 1]`（缓动之前的）。
    pub fn progress(&self) -> f32 {
        if self.duration <= 0.0 {
            1.0
        } else {
            self.elapsed / self.duration
        }
    }

    /// 走完了没有。
    pub fn finished(&self) -> bool {
        self.progress() >= 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_starts_at_zero_and_ends_at_one() {
        for ease in Ease::ALL {
            assert!(
                ease.apply(0.0).abs() < 1e-5,
                "{ease:?}(0) = {}",
                ease.apply(0.0)
            );
            assert!(
                (ease.apply(1.0) - 1.0).abs() < 1e-5,
                "{ease:?}(1) = {}",
                ease.apply(1.0)
            );
        }
    }

    #[test]
    fn in_and_out_are_mirror_images() {
        for (ease_in, ease_out) in [
            (Ease::InQuad, Ease::OutQuad),
            (Ease::InCubic, Ease::OutCubic),
            (Ease::InSine, Ease::OutSine),
        ] {
            for i in 0..=10 {
                let t = i as f32 / 10.0;
                assert!((ease_out.apply(t) - (1.0 - ease_in.apply(1.0 - t))).abs() < 1e-5);
            }
        }
        // 两头慢的曲线在正中间恰好走一半。
        for ease in [
            Ease::InOutQuad,
            Ease::InOutCubic,
            Ease::InOutSine,
            Ease::InOutBack,
        ] {
            assert!((ease.apply(0.5) - 0.5).abs() < 1e-5, "{ease:?}");
        }
    }

    #[test]
    fn back_overshoots_and_bounce_does_not() {
        assert!((0..100).any(|i| Ease::OutBack.apply(i as f32 / 100.0) > 1.0));
        assert!((0..100).all(|i| Ease::OutBounce.apply(i as f32 / 100.0) <= 1.0 + 1e-5));
    }

    #[test]
    fn names_round_trip_and_accept_variants() {
        for ease in Ease::ALL {
            assert_eq!(Ease::from_name(ease.name()), Some(ease));
        }
        assert_eq!(Ease::from_name("easeOutCubic"), Some(Ease::OutCubic));
        assert_eq!(Ease::from_name("ease_in_out_sine"), Some(Ease::InOutSine));
        assert_eq!(Ease::from_name("LINEAR"), Some(Ease::Linear));
        assert_eq!(Ease::from_name("wobbly"), None);
    }

    #[test]
    fn a_tween_clamps_at_the_end_and_handles_zero_duration() {
        let mut tween = Tween::new(kmath::Vec3::ZERO, kmath::Vec3::X, 0.5, Ease::Linear);
        assert_eq!(tween.advance(0.25), kmath::Vec3::new(0.5, 0.0, 0.0));
        assert_eq!(tween.advance(10.0), kmath::Vec3::X);
        assert!(tween.finished());
        let instant = Tween::new(1.0f32, 2.0, 0.0, Ease::OutBounce);
        assert!(instant.finished());
        assert_eq!(instant.value(), 2.0);
    }
}
