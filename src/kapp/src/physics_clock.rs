//! 物理的固定步长时钟。
//!
//! 物理**必须**用定长步长推进。变长步长下同一个场景每次跑出的结果都不一样：
//! 帧率一抖，堆叠的箱子会莫名其妙地塌，弹跳的高度会飘。而渲染的帧间隔天然
//! 是变的，两者对不上，中间就得垫一个累加器。
//!
//! 做法是经典的「固定时间步」：把真实帧间隔攒起来，够一步就走一步，
//! 剩下的零头留到下一帧。

/// 把变长的帧间隔切成定长的物理步。
#[derive(Debug, Clone, Copy)]
pub struct PhysicsClock {
    /// 一步的长度，秒。
    step: f32,
    /// 攒着还没够一步的时间。
    accumulator: f32,
    /// 单帧最多走几步。
    max_steps: u32,
    /// 单帧最多接受多长的真实间隔。
    max_frame_time: f32,
}

impl Default for PhysicsClock {
    fn default() -> Self {
        Self::new(60.0)
    }
}

impl PhysicsClock {
    /// 按每秒 `hz` 步创建。
    pub fn new(hz: f32) -> Self {
        Self {
            step: 1.0 / hz.max(1.0),
            accumulator: 0.0,
            // 上限存在的意义是避免「死亡螺旋」：某一帧卡了 2 秒，
            // 不限制的话这一帧要补 120 步物理，于是卡得更久，于是要补更多步。
            // 宁可让物理慢一点，也不能让它把自己拖死。
            max_steps: 4,
            max_frame_time: 0.25,
        }
    }

    /// 一步的长度。
    pub fn step(&self) -> f32 {
        self.step
    }

    /// 改步频。
    pub fn set_hz(&mut self, hz: f32) {
        self.step = 1.0 / hz.max(1.0);
    }

    /// 单帧最多走几步。
    pub fn set_max_steps(&mut self, max_steps: u32) {
        self.max_steps = max_steps.max(1);
    }

    /// 攒进 `dt` 秒，返回这一帧该走几步。
    ///
    /// 走满上限时会把没消化完的时间**丢掉**——攒着只会让下一帧背更多债。
    /// 表现是物理相对现实时间变慢，但比越卡越卡要好。
    pub fn accumulate(&mut self, dt: f32) -> u32 {
        if !dt.is_finite() || dt <= 0.0 {
            return 0;
        }
        self.accumulator += dt.min(self.max_frame_time);

        let mut steps = 0;
        while self.accumulator >= self.step && steps < self.max_steps {
            self.accumulator -= self.step;
            steps += 1;
        }
        if steps == self.max_steps {
            self.accumulator = 0.0;
        }
        steps
    }

    /// 当前攒着的零头，取值在 `[0, step)`。
    pub fn leftover(&self) -> f32 {
        self.accumulator
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn exactly_one_step_per_frame_at_the_matching_frame_rate() {
        let mut clock = PhysicsClock::new(60.0);
        for _ in 0..10 {
            assert_eq!(clock.accumulate(1.0 / 60.0), 1);
        }
    }

    #[test]
    fn a_high_frame_rate_produces_steps_only_when_enough_time_has_piled_up() {
        // 240 FPS：四帧才够一步物理。
        let mut clock = PhysicsClock::new(60.0);
        let counts: Vec<_> = (0..8).map(|_| clock.accumulate(1.0 / 240.0)).collect();

        assert_eq!(counts.iter().sum::<u32>(), 2);
        assert_eq!(counts, vec![0, 0, 0, 1, 0, 0, 0, 1]);
    }

    #[test]
    fn a_low_frame_rate_catches_up_with_multiple_steps() {
        let mut clock = PhysicsClock::new(60.0);
        // 30 FPS 的一帧 = 两步物理。
        assert_eq!(clock.accumulate(1.0 / 30.0), 2);
    }

    #[test]
    fn a_long_stall_is_capped_instead_of_spiralling() {
        // 卡了两秒。按 60 Hz 补要走 120 步，那会把下一帧也拖垮。
        let mut clock = PhysicsClock::new(60.0);
        assert_eq!(clock.accumulate(2.0), 4);
        // 没消化的时间被丢掉了，下一帧从干净状态开始。
        assert_eq!(clock.leftover(), 0.0);
        assert_eq!(clock.accumulate(1.0 / 60.0), 1);
    }

    #[test]
    fn leftover_time_carries_into_the_next_frame() {
        let mut clock = PhysicsClock::new(60.0);
        clock.accumulate(1.0 / 100.0);
        let first = clock.leftover();
        assert!(first > 0.0 && first < clock.step());

        clock.accumulate(1.0 / 100.0);
        // 两个 0.01 秒加起来超过 1/60，该走一步，零头继续留着。
        assert!(clock.leftover() < clock.step());
    }

    #[test]
    fn no_time_is_created_or_lost() {
        // 真正的不变量：走过的物理时间 + 攒着的零头 = 喂进去的真实时间。
        // 只看「走过多少」会在步的边界上差整整一步，那是零头的正常存在，不是漏账。
        let mut clock = PhysicsClock::new(60.0);
        let mut simulated = 0.0;
        for _ in 0..137 {
            simulated += clock.accumulate(1.0 / 137.0) as f32 * clock.step();
        }

        let accounted = simulated + clock.leftover();
        assert!(
            (accounted - 1.0).abs() < 1e-3,
            "喂进去 1 秒，账上只有 {accounted} 秒"
        );
        // 零头按定义不该攒够一步。
        assert!(clock.leftover() < clock.step());
    }

    #[test]
    fn a_zero_or_bogus_delta_produces_no_steps() {
        let mut clock = PhysicsClock::new(60.0);

        assert_eq!(clock.accumulate(0.0), 0);
        assert_eq!(clock.accumulate(-1.0), 0);
        assert_eq!(clock.accumulate(f32::NAN), 0);
        assert_eq!(clock.leftover(), 0.0);
    }

    #[test]
    fn the_step_length_follows_the_configured_rate() {
        let mut clock = PhysicsClock::new(120.0);
        assert!((clock.step() - 1.0 / 120.0).abs() < 1e-9);

        clock.set_hz(30.0);
        assert!((clock.step() - 1.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn an_absurd_rate_is_clamped_instead_of_dividing_by_zero() {
        let clock = PhysicsClock::new(0.0);
        assert!(clock.step().is_finite());
        assert_eq!(clock.step(), 1.0);
    }
}
