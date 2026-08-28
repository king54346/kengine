//! 屏幕震动。
//!
//! 受击、爆炸、重物落地——想让画面「有分量」时用它。
//!
//! # 核心是一个叫「创伤」的量
//!
//! 事件往里加创伤，创伤随时间衰减，实际抖动幅度取**创伤的平方**。
//! 直接拿一个「剩余时长」去线性衰减也能抖，但收尾时会拖一条
//! 「不痛不痒地抖着」的尾巴，很廉价；平方让大创伤依然剧烈、小创伤迅速
//! 归零，收得干净。（这套做法出自 GDC 那篇讲游戏感的经典分享。）
//!
//! # 它是叠加的，不是覆盖的
//!
//! [`update`](ScreenShake::update) 交出的是一对**偏移量**，加在你原本算好的
//! 相机位姿上。震动不该接管运镜——正在跟随的相机该继续跟随，只是抖着跟。
//!
//! ```
//! # use kcamera::ScreenShake;
//! let mut shake = ScreenShake::default();
//!
//! shake.add_trauma(0.6);                    // 挨了一发
//! let (offset, roll) = shake.update(0.016); // 每帧推进
//! // camera_position += right * offset.x + up * offset.y;
//! // camera_rotation *= Quat::from_rotation_z(roll);
//! ```

use kmath::{Rng, Vec2};

/// 一个屏幕震动器。
#[derive(Debug, Clone)]
pub struct ScreenShake {
    trauma: f32,
    /// 创伤每秒衰减多少。调大收得快。
    pub decay: f32,
    /// 满创伤时的最大平移幅度（世界单位）。
    pub max_offset: f32,
    /// 满创伤时的最大滚转幅度（弧度）。
    ///
    /// 一点点就够：0.1 弧度（约 6°）已经很晃了，再大容易让人晕。
    pub max_roll: f32,
    /// 抖动的随机源。
    ///
    /// 固定种子：同一串事件抖出来的画面每次都一样，录像回放、
    /// 截图比对才对得上。
    rng: Rng,
}

impl Default for ScreenShake {
    fn default() -> Self {
        Self {
            trauma: 0.0,
            decay: 1.5,
            max_offset: 0.4,
            max_roll: 0.1,
            rng: Rng::new(0x5CEE_D00D_u64),
        }
    }
}

impl ScreenShake {
    /// 指定衰减速度与幅度。
    pub fn new(decay: f32, max_offset: f32, max_roll: f32) -> Self {
        Self {
            decay,
            max_offset,
            max_roll,
            ..Self::default()
        }
    }

    /// 换一个随机种子。
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = Rng::new(seed);
        self
    }

    /// 加一次创伤，会被夹在 `0..=1`。
    ///
    /// 手感参考：轻微受击 0.2，爆炸 0.6，剧情级 1.0。
    /// 连续挨打时创伤会累积，但封顶——不会因为连中十发就晃得看不清。
    pub fn add_trauma(&mut self, amount: f32) {
        if !amount.is_finite() {
            return;
        }
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    /// 当前创伤值。
    pub fn trauma(&self) -> f32 {
        self.trauma
    }

    /// 还在抖吗。
    pub fn is_shaking(&self) -> bool {
        self.trauma > 0.0
    }

    /// 立刻停下。
    pub fn stop(&mut self) {
        self.trauma = 0.0;
    }

    /// 推进一帧，返回这一帧的 `(平移偏移, 滚转角)`。
    ///
    /// 不抖的时候返回全零，可以无条件加上去。
    pub fn update(&mut self, dt: f32) -> (Vec2, f32) {
        if self.trauma <= 0.0 {
            return (Vec2::ZERO, 0.0);
        }

        // 平方衰减：见模块文档。
        let amount = self.trauma * self.trauma;
        let offset = Vec2::new(
            self.rng.next_signed() * self.max_offset * amount,
            self.rng.next_signed() * self.max_offset * amount,
        );
        let roll = self.rng.next_signed() * self.max_roll * amount;

        if dt.is_finite() {
            self.trauma = (self.trauma - self.decay * dt).max(0.0);
        }
        (offset, roll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_decays_to_a_full_stop() {
        let mut shake = ScreenShake::default();
        shake.add_trauma(1.0);
        assert!(shake.is_shaking());

        for _ in 0..120 {
            shake.update(1.0 / 60.0);
        }

        assert!(!shake.is_shaking(), "震动没有停下来");
        assert_eq!(shake.update(0.016), (Vec2::ZERO, 0.0), "停了还在给偏移");
    }

    #[test]
    fn the_falloff_is_squared_not_linear() {
        // 平方衰减让小创伤迅速归零，收尾干净；线性的话会拖一条
        // 「不痛不痒地抖着」的尾巴。
        //
        // 判据：创伤减半时幅度该降到四分之一附近，而不是一半。
        let peak = |trauma: f32| {
            let mut shake = ScreenShake {
                decay: 0.0, // 冻住衰减，只看幅度
                ..Default::default()
            };
            shake.add_trauma(trauma);
            let mut peak: f32 = 0.0;
            for _ in 0..300 {
                peak = peak.max(shake.update(0.016).0.x.abs());
            }
            peak
        };

        let full = peak(1.0);
        let half = peak(0.5);
        assert!(
            half < full * 0.35,
            "半创伤幅度 {half}，满创伤 {full}——不像平方衰减"
        );
    }

    #[test]
    fn trauma_accumulates_but_is_capped() {
        // 连中十发不该晃得看不清。
        let mut shake = ScreenShake::default();
        for _ in 0..10 {
            shake.add_trauma(0.5);
        }
        assert_eq!(shake.trauma(), 1.0);
    }

    #[test]
    fn the_offset_stays_within_its_bounds() {
        let mut shake = ScreenShake::default();
        shake.add_trauma(1.0);

        for _ in 0..200 {
            let (offset, roll) = shake.update(0.001);
            assert!(offset.x.abs() <= shake.max_offset + 1e-5);
            assert!(offset.y.abs() <= shake.max_offset + 1e-5);
            assert!(roll.abs() <= shake.max_roll + 1e-5);
        }
    }

    #[test]
    fn it_is_reproducible_from_a_seed() {
        // 固定种子：录像回放、截图比对才对得上。
        let run = || {
            let mut shake = ScreenShake::default();
            shake.add_trauma(1.0);
            (0..10).map(|_| shake.update(0.016)).collect::<Vec<_>>()
        };

        assert_eq!(run(), run());
    }

    #[test]
    fn different_seeds_shake_differently() {
        let run = |seed: u64| {
            let mut shake = ScreenShake::default().with_seed(seed);
            shake.add_trauma(1.0);
            (0..10).map(|_| shake.update(0.016)).collect::<Vec<_>>()
        };

        assert_ne!(run(1), run(2));
    }

    #[test]
    fn a_non_finite_input_does_not_poison_it() {
        // NaN 一旦进了创伤值，`> 0.0` 恒为假，震动就再也触发不了了，
        // 而且不报任何错。
        let mut shake = ScreenShake::default();
        shake.add_trauma(f32::NAN);
        assert_eq!(shake.trauma(), 0.0);

        shake.add_trauma(1.0);
        shake.update(f32::NAN);
        assert!(shake.trauma().is_finite());
    }

    #[test]
    fn stopping_is_immediate() {
        let mut shake = ScreenShake::default();
        shake.add_trauma(1.0);
        shake.stop();

        assert!(!shake.is_shaking());
    }
}
