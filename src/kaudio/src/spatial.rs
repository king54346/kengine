//! 3D 空间音频：距离衰减与声像。
//!
//! 全是纯数学，没有任何硬件依赖——这一层的正确性可以在没有声卡的机器上
//! 完整验证，而它恰恰是「听起来对不对」的主要来源。

use kmath::{Mat4, Vec3};

/// 距离衰减模型。
///
/// 三种都是游戏音频的通行做法，区别只在衰减曲线的形状：
/// 线性可预测但不真实，反比接近物理规律，指数最容易调出「一走远就没了」的效果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Attenuation {
    /// 不衰减。背景音乐、旁白用。
    None,
    /// 线性：从 `min_distance` 到 `max_distance` 均匀降到 0。
    Linear,
    /// 反比：`min / (min + rolloff × (d − min))`。最接近现实中声压随距离的变化。
    #[default]
    Inverse,
    /// 指数：`(d / min)^(−rolloff)`。衰减最快。
    Exponential,
}

/// 一个声源的空间参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spatial {
    /// 衰减模型。
    pub model: Attenuation,
    /// 参考距离。近于此距离时不再变响——否则贴到声源上音量会冲到无穷。
    pub min_distance: f32,
    /// 最远距离。超出后音量为 0（线性模型）或维持在该处的值。
    pub max_distance: f32,
    /// 衰减速率。越大衰减越快。
    pub rolloff: f32,
    /// 声像强度：0 = 完全不做方向定位，1 = 完全按方向摆。
    ///
    /// 调低一点是常见做法——完全硬摆到单侧耳机听起来很不舒服。
    pub panning: f32,
}

impl Default for Spatial {
    fn default() -> Self {
        Self {
            model: Attenuation::Inverse,
            min_distance: 1.0,
            max_distance: 100.0,
            rolloff: 1.0,
            panning: 1.0,
        }
    }
}

impl Spatial {
    /// 不做任何空间处理：音量恒定、居中。背景音乐用。
    pub fn none() -> Self {
        Self {
            model: Attenuation::None,
            panning: 0.0,
            ..Self::default()
        }
    }

    /// 指定衰减范围。
    pub fn with_range(mut self, min_distance: f32, max_distance: f32) -> Self {
        self.min_distance = min_distance.max(1e-3);
        self.max_distance = max_distance.max(self.min_distance);
        self
    }

    /// 指定衰减模型与速率。
    pub fn with_model(mut self, model: Attenuation, rolloff: f32) -> Self {
        self.model = model;
        self.rolloff = rolloff.max(0.0);
        self
    }

    /// 指定声像强度。
    pub fn with_panning(mut self, panning: f32) -> Self {
        self.panning = panning.clamp(0.0, 1.0);
        self
    }

    /// 按距离算出音量倍率，取值在 `[0, 1]`。
    pub fn gain(&self, distance: f32) -> f32 {
        if !distance.is_finite() || distance < 0.0 {
            return 0.0;
        }
        // 近距离夹住：不夹的话贴到声源上音量会冲到无穷，
        // 玩家走过去的一瞬间会被震一下。
        let distance = distance.max(self.min_distance);

        let gain = match self.model {
            Attenuation::None => 1.0,
            Attenuation::Linear => {
                let span = (self.max_distance - self.min_distance).max(1e-6);
                1.0 - self.rolloff * (distance - self.min_distance) / span
            }
            Attenuation::Inverse => {
                self.min_distance
                    / (self.min_distance + self.rolloff * (distance - self.min_distance))
            }
            Attenuation::Exponential => (distance / self.min_distance).powf(-self.rolloff),
        };

        gain.clamp(0.0, 1.0)
    }
}

/// 听者：位置与朝向。
///
/// 通常跟着相机走。朝向用 -Z 前、+Y 上，与本引擎（和 glTF）的相机约定一致。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    /// 世界空间位置。
    pub position: Vec3,
    /// 前方（单位向量）。
    pub forward: Vec3,
    /// 上方（单位向量）。
    pub up: Vec3,
    /// 总音量。
    pub gain: f32,
}

impl Default for Listener {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::NEG_Z,
            up: Vec3::Y,
            gain: 1.0,
        }
    }
}

impl Listener {
    /// 从一个世界变换矩阵取位姿，朝向按 -Z 前、+Y 上。
    pub fn from_matrix(matrix: Mat4) -> Self {
        let forward = -matrix.z_axis.truncate().normalize_or(Vec3::NEG_Z);
        let up = matrix.y_axis.truncate().normalize_or(Vec3::Y);
        Self {
            position: matrix.w_axis.truncate(),
            forward,
            up,
            gain: 1.0,
        }
    }

    /// 听者的右方。
    pub fn right(&self) -> Vec3 {
        self.forward.cross(self.up).normalize_or(Vec3::X)
    }

    /// 声源相对听者的声像位置，`-1` 全左、`0` 居中、`+1` 全右。
    pub fn pan_of(&self, source: Vec3) -> f32 {
        let offset = source - self.position;
        let distance = offset.length();
        // 正好压在听者头上时方向无从谈起，居中处理。
        if distance < 1e-5 {
            return 0.0;
        }
        (offset / distance).dot(self.right()).clamp(-1.0, 1.0)
    }
}

/// 把声像位置换算成左右声道的增益。
///
/// 用**等功率**（正弦-余弦）而不是线性：线性声像在正中间时两声道各 0.5，
/// 总功率只有单侧的一半，声音扫过中间会有一个明显的"塌陷"。
/// 等功率下 `L² + R²` 恒为 1，扫过去响度是平的。
pub fn equal_power_pan(pan: f32) -> [f32; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    // 把 [-1, 1] 映到 [0, π/2]。
    let angle = (pan + 1.0) * 0.25 * std::f32::consts::PI;
    [angle.cos(), angle.sin()]
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn no_attenuation_stays_at_full_volume() {
        let spatial = Spatial::none();

        assert_eq!(spatial.gain(0.0), 1.0);
        assert_eq!(spatial.gain(1000.0), 1.0);
    }

    #[test]
    fn every_model_is_loudest_at_the_reference_distance() {
        for model in [
            Attenuation::Linear,
            Attenuation::Inverse,
            Attenuation::Exponential,
        ] {
            let spatial = Spatial::default().with_model(model, 1.0);
            assert!(
                (spatial.gain(spatial.min_distance) - 1.0).abs() < 1e-5,
                "{model:?}"
            );
        }
    }

    #[test]
    fn getting_closer_than_the_reference_distance_does_not_get_louder() {
        // 不夹的话贴到声源上音量会冲到无穷，玩家走过去会被震一下。
        for model in [
            Attenuation::Linear,
            Attenuation::Inverse,
            Attenuation::Exponential,
        ] {
            let spatial = Spatial::default().with_model(model, 1.0);
            assert_eq!(
                spatial.gain(0.0),
                spatial.gain(spatial.min_distance),
                "{model:?}"
            );
            assert!(spatial.gain(0.001) <= 1.0, "{model:?}");
        }
    }

    #[test]
    fn every_model_decreases_monotonically() {
        for model in [
            Attenuation::Linear,
            Attenuation::Inverse,
            Attenuation::Exponential,
        ] {
            let spatial = Spatial::default()
                .with_range(1.0, 50.0)
                .with_model(model, 1.0);
            let mut previous = f32::MAX;
            for step in 0..200 {
                let gain = spatial.gain(step as f32 * 0.5);
                assert!(gain <= previous + 1e-6, "{model:?} 在 {step} 处反弹了");
                previous = gain;
            }
        }
    }

    #[test]
    fn gain_never_leaves_the_zero_to_one_range() {
        for model in [
            Attenuation::None,
            Attenuation::Linear,
            Attenuation::Inverse,
            Attenuation::Exponential,
        ] {
            let spatial = Spatial::default().with_model(model, 4.0);
            for distance in [0.0, 0.5, 1.0, 10.0, 1e6] {
                let gain = spatial.gain(distance);
                assert!(
                    (0.0..=1.0).contains(&gain),
                    "{model:?} 在 {distance} 处给出 {gain}"
                );
            }
        }
    }

    #[test]
    fn linear_attenuation_reaches_silence_at_the_far_distance() {
        let spatial = Spatial::default()
            .with_range(1.0, 11.0)
            .with_model(Attenuation::Linear, 1.0);

        assert!((spatial.gain(6.0) - 0.5).abs() < 1e-5, "中点该是一半");
        assert_eq!(spatial.gain(11.0), 0.0);
        assert_eq!(spatial.gain(50.0), 0.0);
    }

    #[test]
    fn inverse_attenuation_halves_at_twice_the_reference_distance() {
        // 反比模型的定义就是这条：距离翻倍，声压减半。
        let spatial = Spatial::default()
            .with_range(1.0, 1000.0)
            .with_model(Attenuation::Inverse, 1.0);

        assert!((spatial.gain(2.0) - 0.5).abs() < 1e-5);
        assert!((spatial.gain(4.0) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn a_higher_rolloff_attenuates_faster() {
        let slow = Spatial::default().with_model(Attenuation::Inverse, 0.5);
        let fast = Spatial::default().with_model(Attenuation::Inverse, 4.0);

        assert!(fast.gain(10.0) < slow.gain(10.0));
    }

    #[test]
    fn a_bogus_distance_is_silent_rather_than_nan() {
        let spatial = Spatial::default();

        assert_eq!(spatial.gain(f32::NAN), 0.0);
        assert_eq!(spatial.gain(-1.0), 0.0);
        assert_eq!(spatial.gain(f32::INFINITY), 0.0);
    }

    #[test]
    fn a_degenerate_range_does_not_divide_by_zero() {
        let spatial = Spatial::default()
            .with_range(0.0, 0.0)
            .with_model(Attenuation::Linear, 1.0);

        assert!(spatial.gain(1.0).is_finite());
    }

    // ── 声像 ──

    #[test]
    fn a_centred_source_is_equally_loud_in_both_ears() {
        let [left, right] = equal_power_pan(0.0);

        assert!((left - right).abs() < 1e-6);
        assert!((left * left + right * right - 1.0).abs() < 1e-5);
    }

    #[test]
    fn panning_hard_left_silences_the_right_channel() {
        let [left, right] = equal_power_pan(-1.0);

        assert!((left - 1.0).abs() < 1e-6);
        assert!(right.abs() < 1e-6);
    }

    #[test]
    fn panning_hard_right_silences_the_left_channel() {
        let [left, right] = equal_power_pan(1.0);

        assert!(left.abs() < 1e-6);
        assert!((right - 1.0).abs() < 1e-6);
    }

    #[test]
    fn total_power_is_flat_across_the_whole_sweep() {
        // 线性声像在正中间总功率只有单侧的一半，声音扫过中间会「塌」一下。
        // 等功率的全部意义就是这条曲线是平的。
        for step in 0..=100 {
            let pan = step as f32 / 50.0 - 1.0;
            let [left, right] = equal_power_pan(pan);
            let power = left * left + right * right;
            assert!((power - 1.0).abs() < 1e-5, "pan = {pan} 处功率为 {power}");
        }
    }

    #[test]
    fn panning_is_monotonic_from_left_to_right() {
        let mut previous = f32::MIN;
        for step in 0..=100 {
            let right = equal_power_pan(step as f32 / 50.0 - 1.0)[1];
            assert!(right >= previous - 1e-6);
            previous = right;
        }
    }

    #[test]
    fn out_of_range_pan_is_clamped() {
        assert_eq!(equal_power_pan(-5.0), equal_power_pan(-1.0));
        assert_eq!(equal_power_pan(5.0), equal_power_pan(1.0));
    }

    // ── 听者 ──

    #[test]
    fn a_default_listener_faces_negative_z() {
        let listener = Listener::default();

        assert_eq!(listener.forward, Vec3::NEG_Z);
        // -Z 前、+Y 上 ⇒ 右手是 +X，与相机的约定一致。
        assert!((listener.right() - Vec3::X).length() < 1e-6);
    }

    #[test]
    fn a_source_to_the_right_pans_right() {
        let listener = Listener::default();

        assert!(listener.pan_of(Vec3::X * 5.0) > 0.9);
        assert!(listener.pan_of(Vec3::NEG_X * 5.0) < -0.9);
    }

    #[test]
    fn a_source_straight_ahead_or_behind_is_centred() {
        let listener = Listener::default();

        assert!(listener.pan_of(Vec3::NEG_Z * 10.0).abs() < 1e-5);
        assert!(listener.pan_of(Vec3::Z * 10.0).abs() < 1e-5);
    }

    #[test]
    fn turning_the_listener_swaps_the_sides() {
        // 玩家转身，原本在右边的声音应当跑到左边——这是 3D 音频最基本的一条。
        let source = Vec3::X * 5.0;
        let facing_forward = Listener::default();
        let turned_around = Listener {
            forward: Vec3::Z,
            ..Listener::default()
        };

        assert!(facing_forward.pan_of(source) > 0.9);
        assert!(turned_around.pan_of(source) < -0.9);
    }

    #[test]
    fn a_source_on_top_of_the_listener_is_centred_not_nan() {
        let listener = Listener::default();

        assert_eq!(listener.pan_of(listener.position), 0.0);
    }

    #[test]
    fn a_listener_can_be_read_off_a_camera_matrix() {
        let eye = Vec3::new(3.0, 4.0, 5.0);
        // `look_at` 给的是世界 → 观察空间，取逆才是相机在世界中的位姿。
        let camera_to_world = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y).inverse();
        let listener = Listener::from_matrix(camera_to_world);

        assert!((listener.position - eye).length() < 1e-4);
        // 相机看向原点，前方应当指向原点。
        let expected = (Vec3::ZERO - eye).normalize();
        assert!(
            (listener.forward - expected).length() < 1e-4,
            "{:?}",
            listener.forward
        );
    }

    #[test]
    fn a_degenerate_matrix_falls_back_to_sane_axes() {
        let listener = Listener::from_matrix(Mat4::ZERO);

        assert!(listener.forward.is_finite());
        assert!(listener.right().is_finite());
    }
}
