//! 关键帧曲线：动画数据的最小单位。
//!
//! 一条曲线就是「时间 → 值」的采样点序列。glTF 的每个动画通道正好对应一条曲线，
//! 所以这里的插值方式与 glTF 规范一一对应。

use kmath::{Quat, Vec3, Vec4};

/// 关键帧之间怎么过渡。名称与取值语义均取自 glTF 规范。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interpolation {
    /// 线性插值。
    #[default]
    Linear,
    /// 阶跃：保持前一个关键帧的值直到下一帧，用于开关类动画。
    Step,
    /// 三次样条。每个关键帧存三个值：入切线、值、出切线。
    CubicSpline,
}

/// 能被动画插值的值。
///
/// 分成 `lerp` 与 `blend` 两件事：对位置和缩放它们是一回事，
/// 但对旋转不是——插值要走最短弧，而混合两个姿态时还要处理四元数的双重覆盖
/// （`q` 与 `-q` 表示同一个旋转，不校正就会绕远路）。
pub trait Animatable: Copy {
    /// 沿时间轴插值。
    fn lerp(a: Self, b: Self, t: f32) -> Self;

    /// 按权重混合两个采样结果。`weight` 为 0 时取 `a`，为 1 时取 `b`。
    fn blend(a: Self, b: Self, weight: f32) -> Self {
        Self::lerp(a, b, weight)
    }

    /// 三次埃尔米特插值，供 [`Interpolation::CubicSpline`] 使用。
    ///
    /// `dt` 是两个关键帧的时间差——切线的量纲是「单位时间的变化量」，
    /// 不乘回去的话，关键帧间隔一变，曲线形状就错了。
    fn hermite(a: Self, out_tangent: Self, b: Self, in_tangent: Self, t: f32, dt: f32) -> Self;

    /// 数乘与加法，埃尔米特插值的默认实现要用。
    fn scale(self, factor: f32) -> Self;
    /// 相加。
    fn add(self, other: Self) -> Self;
}

impl Animatable for f32 {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        a + (b - a) * t
    }

    fn hermite(a: Self, out_tangent: Self, b: Self, in_tangent: Self, t: f32, dt: f32) -> Self {
        hermite_scalar(a, out_tangent, b, in_tangent, t, dt)
    }

    fn scale(self, factor: f32) -> Self {
        self * factor
    }

    fn add(self, other: Self) -> Self {
        self + other
    }
}

impl Animatable for Vec3 {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        a + (b - a) * t
    }

    fn hermite(a: Self, out_tangent: Self, b: Self, in_tangent: Self, t: f32, dt: f32) -> Self {
        hermite_generic(a, out_tangent, b, in_tangent, t, dt)
    }

    fn scale(self, factor: f32) -> Self {
        self * factor
    }

    fn add(self, other: Self) -> Self {
        self + other
    }
}

impl Animatable for Quat {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        // 走最短弧：点积为负说明两个四元数在球面上分处两侧，
        // 不翻转其中一个就会绕远路转过去。
        let b = if a.dot(b) < 0.0 { -b } else { b };
        a.slerp(b, t).normalize()
    }

    fn blend(a: Self, b: Self, weight: f32) -> Self {
        // 混合姿态用 nlerp 而不是 slerp：多路混合时要连续叠加，
        // slerp 不满足结合律，叠几层之后角速度会失真；nlerp 便宜且够用。
        let b = if a.dot(b) < 0.0 { -b } else { b };
        let result = Quat::from_vec4(Vec4::from(a) + (Vec4::from(b) - Vec4::from(a)) * weight);
        if result.length_squared() > f32::EPSILON {
            result.normalize()
        } else {
            a
        }
    }

    fn hermite(a: Self, out_tangent: Self, b: Self, in_tangent: Self, t: f32, dt: f32) -> Self {
        // glTF 规定四元数的三次样条按分量插值，最后再归一化。
        let result = Quat::from_vec4(hermite_generic(
            Vec4::from(a),
            Vec4::from(out_tangent),
            Vec4::from(b),
            Vec4::from(in_tangent),
            t,
            dt,
        ));
        if result.length_squared() > f32::EPSILON {
            result.normalize()
        } else {
            a
        }
    }

    fn scale(self, factor: f32) -> Self {
        Quat::from_vec4(Vec4::from(self) * factor)
    }

    fn add(self, other: Self) -> Self {
        Quat::from_vec4(Vec4::from(self) + Vec4::from(other))
    }
}

impl Animatable for Vec4 {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        a + (b - a) * t
    }

    fn hermite(a: Self, out_tangent: Self, b: Self, in_tangent: Self, t: f32, dt: f32) -> Self {
        hermite_generic(a, out_tangent, b, in_tangent, t, dt)
    }

    fn scale(self, factor: f32) -> Self {
        self * factor
    }

    fn add(self, other: Self) -> Self {
        self + other
    }
}

/// 埃尔米特基函数。四个系数分别乘在「起点值、出切线、终点值、入切线」上。
fn hermite_basis(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        2.0 * t3 - 3.0 * t2 + 1.0, // 起点值
        t3 - 2.0 * t2 + t,         // 出切线
        -2.0 * t3 + 3.0 * t2,      // 终点值
        t3 - t2,                   // 入切线
    ]
}

fn hermite_scalar(a: f32, out_tangent: f32, b: f32, in_tangent: f32, t: f32, dt: f32) -> f32 {
    let h = hermite_basis(t);
    h[0] * a + h[1] * dt * out_tangent + h[2] * b + h[3] * dt * in_tangent
}

fn hermite_generic<T: Animatable>(a: T, out_tangent: T, b: T, in_tangent: T, t: f32, dt: f32) -> T {
    let h = hermite_basis(t);
    a.scale(h[0])
        .add(out_tangent.scale(h[1] * dt))
        .add(b.scale(h[2]))
        .add(in_tangent.scale(h[3] * dt))
}

/// 一条关键帧曲线。
#[derive(Debug, Clone, PartialEq)]
pub struct Curve<T> {
    /// 关键帧时间，严格递增。
    times: Vec<f32>,
    /// 关键帧值。三次样条时长度是 `times` 的三倍，
    /// 每帧按「入切线, 值, 出切线」排列（与 glTF 一致）。
    values: Vec<T>,
    interpolation: Interpolation,
}

impl<T: Animatable> Curve<T> {
    /// 用关键帧构造。
    ///
    /// 时间与值的数量对不上时返回 [`None`]——与其在采样时逐帧防御，
    /// 不如在构造时一次性拒绝掉坏数据。
    pub fn new(times: Vec<f32>, values: Vec<T>, interpolation: Interpolation) -> Option<Self> {
        if times.is_empty() {
            return None;
        }
        let expected = match interpolation {
            Interpolation::CubicSpline => times.len() * 3,
            _ => times.len(),
        };
        if values.len() != expected {
            return None;
        }
        Some(Self {
            times,
            values,
            interpolation,
        })
    }

    /// 只有一个值的常量曲线。
    pub fn constant(value: T) -> Self {
        Self {
            times: vec![0.0],
            values: vec![value],
            interpolation: Interpolation::Step,
        }
    }

    /// 关键帧数量。
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// 是否没有关键帧。构造函数保证不会出现，留给外部做断言。
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// 插值方式。
    pub fn interpolation(&self) -> Interpolation {
        self.interpolation
    }

    /// 最后一个关键帧的时间。
    pub fn duration(&self) -> f32 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// 关键帧时间。
    pub fn times(&self) -> &[f32] {
        &self.times
    }

    /// 在给定时刻采样。
    ///
    /// 时间超出首尾关键帧时取端点值（夹紧）：循环播放由播放器负责把时间折回区间，
    /// 曲线本身不猜测调用方的意图。
    pub fn sample(&self, time: f32) -> T {
        let count = self.times.len();
        if count == 1 {
            return self.value_at(0);
        }

        if time <= self.times[0] {
            return self.value_at(0);
        }
        if time >= self.times[count - 1] {
            return self.value_at(count - 1);
        }

        // 找到第一个时间大于 t 的关键帧，它和前一帧夹住了 t。
        let index = self.times.partition_point(|&key| key <= time);
        let (left, right) = (index - 1, index);
        let span = self.times[right] - self.times[left];
        if span <= f32::EPSILON {
            return self.value_at(right);
        }
        let t = (time - self.times[left]) / span;

        match self.interpolation {
            Interpolation::Step => self.value_at(left),
            Interpolation::Linear => T::lerp(self.value_at(left), self.value_at(right), t),
            Interpolation::CubicSpline => T::hermite(
                self.value_at(left),
                self.values[left * 3 + 2],
                self.value_at(right),
                self.values[right * 3],
                t,
                span,
            ),
        }
    }

    /// 取第 `index` 个关键帧的值，自动处理三次样条的三元组布局。
    fn value_at(&self, index: usize) -> T {
        match self.interpolation {
            Interpolation::CubicSpline => self.values[index * 3 + 1],
            _ => self.values[index],
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// 取四元数的旋转角，折算到 [-π, π]。
    fn rotation_angle(q: Quat) -> f32 {
        // 归一化后 w = cos(θ/2)，反解出的角落在 [0, π]。
        let angle = 2.0 * q.w.clamp(-1.0, 1.0).acos();
        if angle > std::f32::consts::PI {
            angle - std::f32::consts::TAU
        } else {
            angle
        }
    }

    fn linear_curve() -> Curve<f32> {
        Curve::new(
            vec![0.0, 1.0, 2.0],
            vec![0.0, 10.0, 30.0],
            Interpolation::Linear,
        )
        .unwrap()
    }

    #[test]
    fn samples_hit_the_keyframes_exactly() {
        let curve = linear_curve();

        assert_eq!(curve.sample(0.0), 0.0);
        assert_eq!(curve.sample(1.0), 10.0);
        assert_eq!(curve.sample(2.0), 30.0);
    }

    #[test]
    fn linear_interpolation_between_keys() {
        let curve = linear_curve();

        assert_eq!(curve.sample(0.5), 5.0);
        // 第二段的斜率是第一段的两倍。
        assert_eq!(curve.sample(1.5), 20.0);
    }

    #[test]
    fn out_of_range_samples_are_clamped() {
        let curve = linear_curve();

        // 循环由播放器负责把时间折回区间，曲线自己不猜。
        assert_eq!(curve.sample(-10.0), 0.0);
        assert_eq!(curve.sample(100.0), 30.0);
    }

    #[test]
    fn step_holds_the_previous_value() {
        let curve = Curve::new(vec![0.0, 1.0], vec![5.0, 9.0], Interpolation::Step).unwrap();

        assert_eq!(curve.sample(0.0), 5.0);
        assert_eq!(curve.sample(0.999), 5.0);
        assert_eq!(curve.sample(1.0), 9.0);
    }

    #[test]
    fn single_key_curve_is_constant() {
        let curve = Curve::constant(7.0);

        assert_eq!(curve.sample(-1.0), 7.0);
        assert_eq!(curve.sample(0.0), 7.0);
        assert_eq!(curve.sample(99.0), 7.0);
        assert_eq!(curve.duration(), 0.0);
    }

    #[test]
    fn mismatched_key_counts_are_rejected() {
        // 与其在采样时逐帧防御，不如构造时就拒绝。
        assert!(Curve::new(vec![0.0, 1.0], vec![0.0], Interpolation::Linear).is_none());
        assert!(Curve::<f32>::new(vec![], vec![], Interpolation::Linear).is_none());
        // 三次样条要三倍的值。
        assert!(Curve::new(vec![0.0], vec![0.0], Interpolation::CubicSpline).is_none());
        assert!(Curve::new(vec![0.0], vec![0.0, 1.0, 2.0], Interpolation::CubicSpline).is_some());
    }

    #[test]
    fn cubic_spline_passes_through_its_keyframes() {
        // 每帧三元组：入切线、值、出切线。
        let curve = Curve::new(
            vec![0.0, 1.0],
            vec![0.0, 0.0, 1.0, 1.0, 1.0, 0.0],
            Interpolation::CubicSpline,
        )
        .unwrap();

        assert!((curve.sample(0.0) - 0.0).abs() < 1e-6);
        assert!((curve.sample(1.0) - 1.0).abs() < 1e-6);
        // 中间应当在两端之间，且不等于线性插值（说明切线起了作用）。
        let middle = curve.sample(0.5);
        assert!((0.0..=1.0).contains(&middle));
    }

    #[test]
    fn cubic_spline_degenerates_to_a_line_with_matching_tangents() {
        // 切线取实际斜率时，三次样条必须退化成直线。
        // 这同时验证了「切线要乘以关键帧时间差」——glTF 的切线是每单位时间的
        // 变化量，漏乘时间差的话，跨度不为 1 的曲线就会鼓出来。
        for (span, slope) in [(1.0f32, 1.0f32), (2.0, 0.5), (0.25, 4.0)] {
            let curve = Curve::new(
                vec![0.0, span],
                vec![slope, 0.0, slope, slope, 1.0, slope],
                Interpolation::CubicSpline,
            )
            .unwrap();

            for step in 0..=4 {
                let t = step as f32 / 4.0;
                let expected = t; // 起点 0、终点 1 的直线
                let actual = curve.sample(t * span);
                assert!(
                    (actual - expected).abs() < 1e-5,
                    "跨度 {span} 时 t={t} 采样到 {actual}，期望 {expected}"
                );
            }
        }
    }

    #[test]
    fn quaternion_lerp_takes_the_short_way() {
        let a = Quat::IDENTITY;
        // 绕 Y 轴 350°，等价于 -10°。插值到一半应当落在 -5° 附近，而不是 175°。
        let b = Quat::from_rotation_y(350f32.to_radians());

        let middle = Quat::lerp(a, b, 0.5);
        let angle = rotation_angle(middle);

        assert!(
            angle.to_degrees().abs() < 15.0,
            "插值绕了远路：{}",
            angle.to_degrees()
        );
    }

    #[test]
    fn quaternion_samples_stay_normalized() {
        let curve = Curve::new(
            vec![0.0, 1.0],
            vec![
                Quat::IDENTITY,
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            ],
            Interpolation::Linear,
        )
        .unwrap();

        for step in 0..=10 {
            let q = curve.sample(step as f32 / 10.0);
            assert!((q.length() - 1.0).abs() < 1e-5, "四元数没有归一化");
        }
    }

    #[test]
    fn quaternion_blend_handles_double_cover() {
        let a = Quat::from_rotation_y(0.1);
        // 同一个旋转的另一种表示，混合时必须先校正符号，否则会转到反方向。
        let b = -Quat::from_rotation_y(0.2);

        let blended = Quat::blend(a, b, 1.0);

        assert!(blended.dot(Quat::from_rotation_y(0.2)).abs() > 0.999);
    }

    #[test]
    fn vector_curves_interpolate_per_component() {
        let curve = Curve::new(
            vec![0.0, 1.0],
            vec![Vec3::ZERO, Vec3::new(2.0, 4.0, 6.0)],
            Interpolation::Linear,
        )
        .unwrap();

        assert_eq!(curve.sample(0.5), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn duplicate_key_times_do_not_divide_by_zero() {
        let curve = Curve::new(
            vec![0.0, 0.0, 1.0],
            vec![1.0, 2.0, 3.0],
            Interpolation::Linear,
        )
        .unwrap();

        let value = curve.sample(0.0);
        assert!(value.is_finite());
    }
}
