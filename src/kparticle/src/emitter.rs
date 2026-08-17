//! 发射器：决定粒子在**哪里**出生、朝**哪个方向**、带什么初始参数。
//!
//! 形状只管位置，方向单独由 `direction` + `spread` 描述。两者拆开是有意的：
//! 「从一个球里往上喷」和「从一个球里四散」是两种常见效果，
//! 把方向绑死在形状上就表达不了第一种。

use crate::rng::{Rng, Span};
use kmath::Vec3;

/// 粒子出生位置的分布形状，以发射器自身的局部坐标为准。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EmitterShape {
    /// 一个点，全部粒子从同一处出生。
    Point,
    /// 球体内部均匀分布。
    Sphere {
        /// 半径。
        radius: f32,
    },
    /// 长方体内部均匀分布。
    Box {
        /// 各轴半长。
        half_extents: Vec3,
    },
    /// XZ 平面上的圆盘内部均匀分布，适合地面上的烟雾、光环。
    Disk {
        /// 半径。
        radius: f32,
    },
}

impl Default for EmitterShape {
    fn default() -> Self {
        Self::Point
    }
}

impl EmitterShape {
    /// 在形状内取一个出生点。
    pub fn sample(&self, rng: &mut Rng) -> Vec3 {
        match *self {
            Self::Point => Vec3::ZERO,
            Self::Sphere { radius } => rng.in_unit_sphere() * radius,
            Self::Box { half_extents } => rng.in_unit_cube() * half_extents,
            Self::Disk { radius } => rng.in_unit_disk() * radius,
        }
    }

    /// 形状本身的包围盒，用来给粒子系统的包围盒兜底。
    pub fn bounds(&self) -> Vec3 {
        match *self {
            Self::Point => Vec3::ZERO,
            Self::Sphere { radius } => Vec3::splat(radius.abs()),
            Self::Box { half_extents } => half_extents.abs(),
            Self::Disk { radius } => Vec3::new(radius.abs(), 0.0, radius.abs()),
        }
    }
}

/// 一个发射器。
///
/// 默认值是一团向上飘、两秒消散的小粒子，直接用也能看到东西。
#[derive(Debug, Clone)]
pub struct Emitter {
    /// 发射器相对所在节点的位置。
    pub position: Vec3,
    /// 出生位置的分布形状。
    pub shape: EmitterShape,
    /// 每秒生成多少个粒子。
    pub rate: f32,
    /// 一次性喷发的数量，[`ParticleSystem::burst`](crate::ParticleSystem::burst) 用。
    pub burst: u32,
    /// 初始速度方向的中轴。
    pub direction: Vec3,
    /// 方向的发散半角（弧度）。0 表示笔直，π 表示全向。
    pub spread: f32,
    /// 初速大小的取值范围。
    pub speed: Span,
    /// 寿命的取值范围（秒）。
    pub lifetime: Span,
    /// 初始尺寸的取值范围。
    pub size: Span,
    /// 初始朝向的取值范围（弧度，绕视线轴）。
    pub rotation: Span,
    /// 自转速度的取值范围（弧度/秒）。
    pub rotation_speed: Span,
}

impl Default for Emitter {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            shape: EmitterShape::Point,
            rate: 32.0,
            burst: 0,
            direction: Vec3::Y,
            spread: 0.4,
            speed: Span::new(1.0, 2.0),
            lifetime: Span::new(1.0, 2.0),
            size: Span::new(0.1, 0.2),
            rotation: Span::new(-std::f32::consts::PI, std::f32::consts::PI),
            rotation_speed: Span::new(-1.0, 1.0),
        }
    }
}

impl Emitter {
    /// 一个球形分布、四散喷射的发射器。
    pub fn sphere(radius: f32) -> Self {
        Self {
            shape: EmitterShape::Sphere { radius },
            spread: std::f32::consts::PI,
            ..Default::default()
        }
    }

    /// 一个向上喷的锥形发射器，`spread_degrees` 是发散半角（角度制）。
    pub fn cone(spread_degrees: f32) -> Self {
        Self {
            spread: spread_degrees.to_radians(),
            ..Default::default()
        }
    }

    /// 一个圆盘分布的发射器，常用于地面烟雾。
    pub fn disk(radius: f32) -> Self {
        Self {
            shape: EmitterShape::Disk { radius },
            ..Default::default()
        }
    }

    /// 指定位置。
    pub fn with_position(mut self, position: Vec3) -> Self {
        self.position = position;
        self
    }

    /// 指定每秒生成数。
    pub fn with_rate(mut self, rate: f32) -> Self {
        self.rate = rate;
        self
    }

    /// 指定喷射方向。
    pub fn with_direction(mut self, direction: Vec3) -> Self {
        self.direction = direction;
        self
    }

    /// 指定发散半角（角度制）。
    pub fn with_spread_degrees(mut self, degrees: f32) -> Self {
        self.spread = degrees.to_radians();
        self
    }

    /// 指定初速范围。
    pub fn with_speed(mut self, speed: impl Into<Span>) -> Self {
        self.speed = speed.into();
        self
    }

    /// 指定寿命范围。
    pub fn with_lifetime(mut self, lifetime: impl Into<Span>) -> Self {
        self.lifetime = lifetime.into();
        self
    }

    /// 指定初始尺寸范围。
    pub fn with_size(mut self, size: impl Into<Span>) -> Self {
        self.size = size.into();
        self
    }

    /// 指定自转速度范围。
    pub fn with_rotation_speed(mut self, speed: impl Into<Span>) -> Self {
        self.rotation_speed = speed.into();
        self
    }

    /// 掷一个新粒子的初始状态。
    pub fn spawn(&self, rng: &mut Rng) -> Spawn {
        // 顺序固定：位置 → 方向 → 速度 → ……
        // 一旦调换，同一个种子给出的效果就变了，回归测试也就失去意义。
        let position = self.position + self.shape.sample(rng);
        let direction = rng.in_cone(self.direction, self.spread);
        let speed = self.speed.sample(rng);
        let lifetime = self.lifetime.sample(rng).max(f32::EPSILON);
        let size = self.size.sample(rng).max(0.0);
        let rotation = self.rotation.sample(rng);
        let rotation_speed = self.rotation_speed.sample(rng);

        Spawn {
            position,
            velocity: direction * speed,
            lifetime,
            size,
            rotation,
            rotation_speed,
        }
    }
}

/// 一个刚出生的粒子的初始状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spawn {
    /// 出生位置（发射器局部坐标）。
    pub position: Vec3,
    /// 初速度。
    pub velocity: Vec3,
    /// 总寿命（秒）。
    pub lifetime: f32,
    /// 初始尺寸。
    pub size: f32,
    /// 初始朝向（弧度）。
    pub rotation: f32,
    /// 自转速度（弧度/秒）。
    pub rotation_speed: f32,
}

/// 按生成速率累积时间，决定这一帧该生成几个粒子。
///
/// 单独拎出来是因为它有个容易写错的地方：不能用 `(rate * dt) as u32`——
/// 每秒 10 个、每帧 1/60 秒时那是 `0.166 → 0` 个，粒子永远出不来。
/// 必须把不足一个的零头**留到下一帧**继续攒。
#[derive(Debug, Clone, Copy, Default)]
pub struct SpawnClock {
    /// 攒下的、还不够生成一个粒子的时间。
    debt: f32,
}

impl SpawnClock {
    /// 推进 `dt` 秒，返回这一帧应当生成的粒子数。
    pub fn tick(&mut self, rate: f32, dt: f32) -> u32 {
        if rate <= 0.0 || dt <= 0.0 {
            return 0;
        }

        self.debt += dt * rate;
        // 一帧卡了很久时，debt 可能大得离谱；上限交给调用方的容量限制去截。
        let count = self.debt.floor();
        self.debt -= count;
        count.max(0.0).min(u32::MAX as f32) as u32
    }

    /// 清空攒下的零头。
    pub fn reset(&mut self) {
        self.debt = 0.0;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn slow_rate_still_spawns_eventually() {
        // 每秒 10 个、每帧 1/60 秒：单帧不足一个，但一秒后必须正好 10 个。
        let mut clock = SpawnClock::default();
        let total: u32 = (0..60).map(|_| clock.tick(10.0, 1.0 / 60.0)).sum();

        assert_eq!(total, 10);
    }

    #[test]
    fn spawn_count_matches_rate_over_time() {
        let mut clock = SpawnClock::default();
        let total: u32 = (0..300).map(|_| clock.tick(60.0, 1.0 / 60.0)).sum();

        assert_eq!(total, 300);
    }

    #[test]
    fn zero_rate_never_spawns() {
        let mut clock = SpawnClock::default();

        assert_eq!(clock.tick(0.0, 1.0), 0);
        assert_eq!(clock.tick(-5.0, 1.0), 0);
    }

    #[test]
    fn non_positive_dt_does_not_spawn() {
        let mut clock = SpawnClock::default();

        assert_eq!(clock.tick(100.0, 0.0), 0);
        assert_eq!(clock.tick(100.0, -1.0), 0);
    }

    #[test]
    fn a_long_frame_spawns_the_whole_backlog() {
        let mut clock = SpawnClock::default();

        // 卡了半秒，每秒 100 个 → 这一帧补上 50 个。
        assert_eq!(clock.tick(100.0, 0.5), 50);
    }

    #[test]
    fn reset_drops_the_accumulated_remainder() {
        let mut clock = SpawnClock::default();
        clock.tick(10.0, 0.09);
        clock.reset();

        // 零头被清掉了，再走 0.09 秒仍然攒不满一个。
        assert_eq!(clock.tick(10.0, 0.09), 0);
    }

    #[test]
    fn point_shape_spawns_at_the_origin() {
        let mut rng = Rng::new(1);

        assert_eq!(EmitterShape::Point.sample(&mut rng), Vec3::ZERO);
    }

    #[test]
    fn shapes_contain_their_samples() {
        let mut rng = Rng::new(2);
        let shapes = [
            EmitterShape::Sphere { radius: 3.0 },
            EmitterShape::Box {
                half_extents: Vec3::new(1.0, 2.0, 3.0),
            },
            EmitterShape::Disk { radius: 2.0 },
        ];

        for shape in shapes {
            for _ in 0..500 {
                let point = shape.sample(&mut rng);
                let bounds = shape.bounds();
                assert!(
                    point.abs().cmple(bounds + Vec3::splat(1e-5)).all(),
                    "{shape:?} 采样到 {point:?}，超出自身包围盒 {bounds:?}"
                );
            }
        }
    }

    #[test]
    fn emitter_position_offsets_every_spawn() {
        let emitter = Emitter::default().with_position(Vec3::new(10.0, 0.0, 0.0));
        let mut rng = Rng::new(3);

        // 点发射器 + 位置偏移：所有粒子都从那个偏移处出生。
        for _ in 0..32 {
            assert_eq!(emitter.spawn(&mut rng).position, Vec3::new(10.0, 0.0, 0.0));
        }
    }

    #[test]
    fn spawned_speed_stays_within_range() {
        let emitter = Emitter::default().with_speed((2.0, 5.0));
        let mut rng = Rng::new(4);

        for _ in 0..500 {
            let speed = emitter.spawn(&mut rng).velocity.length();
            assert!((2.0 - 1e-4..=5.0 + 1e-4).contains(&speed), "初速 {speed} 越界");
        }
    }

    #[test]
    fn spawned_direction_respects_spread() {
        let emitter = Emitter::cone(15.0).with_direction(Vec3::Y);
        let mut rng = Rng::new(5);

        for _ in 0..500 {
            let direction = emitter.spawn(&mut rng).velocity.normalize();
            let angle = direction.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(angle <= 15.0 + 1e-3, "偏离 {angle} 度，超出发散角");
        }
    }

    #[test]
    fn lifetime_is_never_zero() {
        // 寿命为 0 会让「年龄 / 寿命」除以零，曲线取样直接变 NaN。
        let emitter = Emitter::default().with_lifetime(0.0);
        let mut rng = Rng::new(6);

        let spawn = emitter.spawn(&mut rng);
        assert!(spawn.lifetime > 0.0);
    }

    #[test]
    fn spawning_is_deterministic() {
        let emitter = Emitter::sphere(2.0);

        let mut a = Rng::new(7);
        let mut b = Rng::new(7);

        for _ in 0..64 {
            assert_eq!(emitter.spawn(&mut a), emitter.spawn(&mut b));
        }
    }
}
