//! 关键帧曲线：粒子的颜色与大小随寿命变化。
//!
//! 一条曲线就是一串 `(时间, 值)` 关键帧，时间归一化到 `[0, 1]`——
//! 0 是粒子刚出生，1 是它咽气的那一刻。这样同一条曲线可以套在寿命不同的粒子上。

use crate::rng::Lerp;
use kmath::{Vec3, Vec4};

/// 一条按时间插值的关键帧曲线。
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient<T> {
    /// 关键帧，构造时已按时间排好序。
    keys: Vec<(f32, T)>,
}

impl<T: Lerp> Gradient<T> {
    /// 常量曲线：全程都是同一个值。
    pub fn constant(value: T) -> Self {
        Self {
            keys: vec![(0.0, value)],
        }
    }

    /// 从起点线性过渡到终点。
    pub fn linear(from: T, to: T) -> Self {
        Self {
            keys: vec![(0.0, from), (1.0, to)],
        }
    }

    /// 用一串关键帧构造。
    ///
    /// 传入顺序无所谓，内部会排序；时间会夹到 `[0, 1]`。
    /// 关键帧为空时退化成常量曲线，值取 `fallback`。
    pub fn new(keys: impl IntoIterator<Item = (f32, T)>, fallback: T) -> Self {
        let mut keys: Vec<(f32, T)> = keys
            .into_iter()
            .map(|(time, value)| (time.clamp(0.0, 1.0), value))
            .collect();
        // NaN 时间会让排序结果不确定，先前面已经夹过了，这里用 total_cmp 兜底。
        keys.sort_by(|a, b| a.0.total_cmp(&b.0));

        if keys.is_empty() {
            keys.push((0.0, fallback));
        }
        Self { keys }
    }

    /// 追加一个关键帧，保持有序。
    pub fn with_key(mut self, time: f32, value: T) -> Self {
        let time = time.clamp(0.0, 1.0);
        let position = self.keys.partition_point(|(t, _)| *t <= time);
        self.keys.insert(position, (time, value));
        self
    }

    /// 关键帧列表。
    pub fn keys(&self) -> &[(f32, T)] {
        &self.keys
    }

    /// 求某个时刻的值。
    ///
    /// 超出首尾关键帧的时间取端点值（夹紧而非外插）——外插会让颜色跑出值域。
    pub fn sample(&self, time: f32) -> T {
        // 构造函数保证了至少有一个关键帧。
        let first = &self.keys[0];
        if time <= first.0 || self.keys.len() == 1 {
            return first.1;
        }
        let last = &self.keys[self.keys.len() - 1];
        if time >= last.0 {
            return last.1;
        }

        // 找到第一个时间大于 t 的关键帧，它和前一帧夹住了 t。
        let index = self.keys.partition_point(|(key_time, _)| *key_time <= time);
        let (left_time, left) = self.keys[index - 1];
        let (right_time, right) = self.keys[index];

        let span = right_time - left_time;
        if span <= f32::EPSILON {
            // 两帧时间重合：直接跳到右值，形成一个硬切换。
            return right;
        }
        left.lerp(right, (time - left_time) / span)
    }
}

impl<T: Lerp> From<T> for Gradient<T> {
    fn from(value: T) -> Self {
        Self::constant(value)
    }
}

/// 颜色随寿命变化的曲线，分量为线性空间的 RGBA。
pub type ColorGradient = Gradient<Vec4>;

/// 标量随寿命变化的曲线，粒子大小用它。
pub type Curve = Gradient<f32>;

impl ColorGradient {
    /// 常用效果：颜色不变，只在末尾淡出。
    pub fn fade_out(color: Vec3) -> Self {
        Self::linear(color.extend(1.0), color.extend(0.0))
    }

    /// 常用效果：先淡入、再淡出。`peak` 是最亮的时刻。
    pub fn fade_in_out(color: Vec3, peak: f32) -> Self {
        Self::new(
            [
                (0.0, color.extend(0.0)),
                (peak, color.extend(1.0)),
                (1.0, color.extend(0.0)),
            ],
            color.extend(1.0),
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn constant_curve_is_flat() {
        let curve = Curve::constant(3.0);

        for t in [0.0, 0.25, 0.5, 1.0, 2.0, -1.0] {
            assert_eq!(curve.sample(t), 3.0);
        }
    }

    #[test]
    fn linear_curve_interpolates() {
        let curve = Curve::linear(0.0, 10.0);

        assert_eq!(curve.sample(0.0), 0.0);
        assert_eq!(curve.sample(0.5), 5.0);
        assert_eq!(curve.sample(1.0), 10.0);
    }

    #[test]
    fn samples_outside_the_range_are_clamped() {
        let curve = Curve::linear(2.0, 4.0);

        // 外插会让颜色跑出值域，所以两端一律夹紧。
        assert_eq!(curve.sample(-5.0), 2.0);
        assert_eq!(curve.sample(5.0), 4.0);
    }

    #[test]
    fn keys_are_sorted_regardless_of_input_order() {
        let curve = Curve::new([(1.0, 30.0), (0.0, 10.0), (0.5, 20.0)], 0.0);

        assert_eq!(
            curve.keys().iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            vec![0.0, 0.5, 1.0]
        );
        assert_eq!(curve.sample(0.25), 15.0);
        assert_eq!(curve.sample(0.75), 25.0);
    }

    #[test]
    fn empty_curve_falls_back_to_a_constant() {
        // 空曲线不能让取样 panic，否则一个配置失误就会打挂整个渲染。
        let curve = Curve::new([], 7.0);

        assert_eq!(curve.sample(0.5), 7.0);
    }

    #[test]
    fn key_times_are_clamped_into_the_unit_range() {
        let curve = Curve::new([(-2.0, 1.0), (3.0, 5.0)], 0.0);

        assert_eq!(curve.keys()[0].0, 0.0);
        assert_eq!(curve.keys()[1].0, 1.0);
    }

    #[test]
    fn duplicate_key_times_produce_a_hard_switch() {
        // 两个关键帧时间相同：应当在该点直接跳变，而不是除以零。
        let curve = Curve::new([(0.0, 0.0), (0.5, 1.0), (0.5, 9.0), (1.0, 9.0)], 0.0);

        let value = curve.sample(0.5);
        assert!(value.is_finite());
        assert_eq!(curve.sample(0.75), 9.0);
    }

    #[test]
    fn with_key_keeps_the_curve_sorted() {
        let curve = Curve::linear(0.0, 1.0).with_key(0.5, 0.9);

        assert_eq!(
            curve.keys().iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            vec![0.0, 0.5, 1.0]
        );
        assert_eq!(curve.sample(0.25), 0.45);
    }

    #[test]
    fn color_gradient_interpolates_every_channel() {
        let gradient =
            ColorGradient::linear(Vec4::new(1.0, 0.0, 0.0, 1.0), Vec4::new(0.0, 1.0, 0.0, 0.0));

        let middle = gradient.sample(0.5);

        assert_eq!(middle, Vec4::new(0.5, 0.5, 0.0, 0.5));
    }

    #[test]
    fn fade_out_keeps_color_and_drops_alpha() {
        let gradient = ColorGradient::fade_out(Vec3::new(1.0, 0.5, 0.0));

        assert_eq!(gradient.sample(0.0).w, 1.0);
        assert_eq!(gradient.sample(1.0).w, 0.0);
        // 只有透明度变，色相不该跟着漂。
        assert_eq!(gradient.sample(0.5).truncate(), Vec3::new(1.0, 0.5, 0.0));
    }

    #[test]
    fn fade_in_out_peaks_where_asked() {
        let gradient = ColorGradient::fade_in_out(Vec3::ONE, 0.25);

        assert_eq!(gradient.sample(0.0).w, 0.0);
        assert_eq!(gradient.sample(0.25).w, 1.0);
        assert_eq!(gradient.sample(1.0).w, 0.0);
        // 峰值两侧都应当低于峰值。
        assert!(gradient.sample(0.1).w < 1.0);
        assert!(gradient.sample(0.6).w < 1.0);
    }
}
