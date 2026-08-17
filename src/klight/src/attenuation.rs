//! 衰减曲线的 CPU 实现。
//!
//! 与 [`crate::LIGHT_WGSL`] 里的同名函数一一对应。着色器没法单元测试，
//! 这份可以——衰减曲线的单调性、边界值、除零安全都在这里断言。
//! 改动其中一份时另一份也要跟着改。

/// 距离衰减：平方反比 + 在 `range` 处平滑归零的窗函数。
pub fn distance(distance: f32, range: f32) -> f32 {
    if range <= 0.0 {
        return 0.0;
    }

    // 分母加 1 避免距离趋零时除爆。
    let falloff = 1.0 / (1.0 + distance * distance);

    let ratio = (distance / range).clamp(0.0, 1.0);
    let ratio2 = ratio * ratio;
    let window = (1.0 - ratio2 * ratio2).clamp(0.0, 1.0);

    falloff * window * window
}

/// 聚光灯锥形衰减。参数为夹角余弦，非角度。
pub fn spot(cos_angle: f32, cos_inner: f32, cos_outer: f32) -> f32 {
    let denominator = cos_inner - cos_outer;
    if denominator <= 1e-5 {
        // 内外锥重合，退化成硬边缘。
        return if cos_angle >= cos_outer { 1.0 } else { 0.0 };
    }

    let t = ((cos_angle - cos_outer) / denominator).clamp(0.0, 1.0);
    t * t
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn attenuation_is_maximal_at_zero_distance() {
        // 距离为 0 时不应除零，且取到最大值。
        let value = distance(0.0, 10.0);

        assert!(value.is_finite());
        assert!((value - 1.0).abs() < 1e-5);
    }

    #[test]
    fn attenuation_reaches_zero_at_range() {
        // 到达作用半径时必须精确归零，否则远处光源永远参与计算。
        assert_eq!(distance(10.0, 10.0), 0.0);
        assert_eq!(distance(20.0, 10.0), 0.0);
    }

    #[test]
    fn attenuation_decreases_monotonically() {
        let range = 10.0;
        let mut previous = distance(0.0, range);

        for i in 1..=100 {
            let current = distance(i as f32 / 10.0, range);
            assert!(
                current <= previous + 1e-6,
                "衰减应单调递减，在 d={} 处回升", i as f32 / 10.0
            );
            previous = current;
        }
    }

    #[test]
    fn attenuation_is_never_negative() {
        for i in 0..200 {
            let d = i as f32 / 10.0;
            assert!(distance(d, 10.0) >= 0.0);
        }
    }

    #[test]
    fn zero_range_yields_no_light() {
        // 半径为 0 的光源不该照亮任何东西，也不能产生 NaN。
        assert_eq!(distance(0.0, 0.0), 0.0);
        assert_eq!(distance(5.0, 0.0), 0.0);
        assert_eq!(distance(0.0, -1.0), 0.0);
    }

    #[test]
    fn spot_is_full_inside_inner_cone() {
        let cos_inner = 20f32.to_radians().cos();
        let cos_outer = 40f32.to_radians().cos();

        // 夹角 10° 在内锥之内。
        let value = spot(10f32.to_radians().cos(), cos_inner, cos_outer);
        assert!((value - 1.0).abs() < 1e-5);
    }

    #[test]
    fn spot_is_zero_outside_outer_cone() {
        let cos_inner = 20f32.to_radians().cos();
        let cos_outer = 40f32.to_radians().cos();

        // 夹角 50° 在外锥之外。
        assert_eq!(spot(50f32.to_radians().cos(), cos_inner, cos_outer), 0.0);
    }

    #[test]
    fn spot_transitions_smoothly_between_cones() {
        let cos_inner = 20f32.to_radians().cos();
        let cos_outer = 40f32.to_radians().cos();

        let value = spot(30f32.to_radians().cos(), cos_inner, cos_outer);

        // 过渡带内应当严格介于 0 和 1 之间。
        assert!(value > 0.0 && value < 1.0, "过渡值 {value} 不在开区间内");
    }

    #[test]
    fn spot_degenerate_cone_does_not_divide_by_zero() {
        // 内外锥完全重合。
        let cos = 30f32.to_radians().cos();

        assert_eq!(spot(cos, cos, cos), 1.0);
        assert_eq!(spot(0.0, cos, cos), 0.0);
    }

    #[test]
    fn spot_is_monotonic_across_transition() {
        let cos_inner = 15f32.to_radians().cos();
        let cos_outer = 45f32.to_radians().cos();
        let mut previous = 1.0;

        // 夹角从 0° 增大到 60°，衰减应当单调递减。
        for degrees in 0..=60 {
            let current = spot((degrees as f32).to_radians().cos(), cos_inner, cos_outer);
            assert!(current <= previous + 1e-6, "在 {degrees}° 处衰减回升");
            previous = current;
        }
    }
}
