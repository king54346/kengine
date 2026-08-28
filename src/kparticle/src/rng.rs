//! 粒子的随机参数：取值区间与插值。
//!
//! 随机数发生器本身已经搬到 [`kmath::Rng`]——它是数学基础设施，不该只有
//! 粒子系统找得到。这里重导出它，粒子这边的代码与外部调用方都不受影响。

use kmath::{Vec3, Vec4};

pub use kmath::Rng;

/// 一个闭区间，粒子的初始参数都从这里取随机值。
///
/// 比 [`std::ops::Range`] 好用的地方：它是 [`Copy`] 的，而且允许 `min == max`
/// 表示「固定值」，不必为常量参数再开一种类型。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// 下界。
    pub min: f32,
    /// 上界。
    pub max: f32,
}

impl Span {
    /// 一个区间。传入顺序颠倒也没关系，会自动摆正。
    pub fn new(min: f32, max: f32) -> Self {
        Self {
            min: min.min(max),
            max: min.max(max),
        }
    }

    /// 固定值，不随机。
    pub fn exact(value: f32) -> Self {
        Self {
            min: value,
            max: value,
        }
    }

    /// 从区间里取一个随机值。
    pub fn sample(&self, rng: &mut Rng) -> f32 {
        if self.min == self.max {
            self.min
        } else {
            self.min + (self.max - self.min) * rng.next_f32()
        }
    }
}

impl From<f32> for Span {
    fn from(value: f32) -> Self {
        Self::exact(value)
    }
}

impl From<(f32, f32)> for Span {
    fn from((min, max): (f32, f32)) -> Self {
        Self::new(min, max)
    }
}

/// 可线性插值的值，用于关键帧曲线。
pub trait Lerp: Copy {
    /// 在两值之间按 `t ∈ [0, 1]` 插值。
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for Vec4 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for Vec3 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn span_orders_its_bounds() {
        let span = Span::new(5.0, 1.0);

        assert_eq!(span.min, 1.0);
        assert_eq!(span.max, 5.0);
    }

    #[test]
    fn span_samples_stay_within_bounds() {
        let mut rng = Rng::new(37);
        let span = Span::new(-2.0, 3.0);

        for _ in 0..1000 {
            let value = span.sample(&mut rng);
            assert!((-2.0..=3.0).contains(&value));
        }
    }

    #[test]
    fn exact_span_does_not_consume_randomness() {
        let mut rng = Rng::new(41);
        let before = rng.clone().next_u32();

        assert_eq!(Span::exact(2.5).sample(&mut rng), 2.5);
        // 固定值不该动用随机数，否则同一套参数换个写法就会改变整个序列。
        assert_eq!(rng.next_u32(), before);
    }
}
