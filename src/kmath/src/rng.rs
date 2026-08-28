//! 确定性伪随机数发生器。
//!
//! 除了原始的随机数，还带几个**几何采样**的基本款（单位球面、球内、
//! 圆盘内、圆锥内）——它们是采样各种形状时反复要用的底料，
//! 见 [`ShapeSample`](crate::ShapeSample)。
//!
//! 引擎自带一个而不是用 `rand`：**确定性是可测试性的前提**——同一个种子
//! 跑两遍必须逐字节一致，否则「这个系统行为对不对」只能靠肉眼看。
//! 每个使用者持有自己的发生器，互不干扰，也就不需要全局状态。
//!
//! 它原先住在 `kparticle`（粒子的初速、方向都要随机），但随机数是数学
//! 基础设施——没人会去粒子库里找它。`kparticle` 仍然重导出这个类型，
//! 那边的代码一行都不用改。

use crate::Vec3;

/// PCG32 随机数发生器。
///
/// 选 PCG 而非线性同余：后者低位的随机性很差，而「取模到小范围」恰好只看低位。
#[derive(Debug, Clone)]
pub struct Rng {
    seed: u64,
    state: u64,
    increment: u64,
}

impl Default for Rng {
    fn default() -> Self {
        Self::new(0x853C_49E6_748F_EA9B)
    }
}

impl Rng {
    /// 用给定种子创建。同种子必然给出同一串数。
    pub fn new(seed: u64) -> Self {
        let mut rng = Self {
            seed,
            state: 0,
            increment: (seed << 1) | 1,
        };
        rng.reset();
        rng
    }

    /// 回到初始状态，用于重放一段模拟。
    pub fn reset(&mut self) {
        self.state = 0;
        self.next_u32();
        self.state = self.state.wrapping_add(self.seed);
        self.next_u32();
    }

    /// 下一个 32 位随机数。
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.increment);
        // 输出函数：先异或折叠高位，再按高位决定的位数循环右移。
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rotation = (old >> 59) as u32;
        xorshifted.rotate_right(rotation)
    }

    /// `[0, 1)` 上的随机数。
    pub fn next_f32(&mut self) -> f32 {
        // 只取高 24 位：f32 的尾数就这么宽，多取的位反正会被舍掉。
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// `[-1, 1)` 上的随机数。
    pub fn next_signed(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }

    /// 单位球面上均匀分布的方向。
    ///
    /// 关键是 `z` 要在 `[-1, 1]` 上**均匀**取，而不是均匀取俯仰角——
    /// 后者会让粒子在两极堆积。
    pub fn unit_vector(&mut self) -> Vec3 {
        let z = self.next_signed();
        let angle = self.next_f32() * std::f32::consts::TAU;
        let radius = (1.0 - z * z).max(0.0).sqrt();
        Vec3::new(radius * angle.cos(), radius * angle.sin(), z)
    }

    /// 单位球**内部**均匀分布的一点。
    ///
    /// 半径要开三次方：体积随半径三次方增长，直接取均匀半径会让粒子挤在球心。
    pub fn in_unit_sphere(&mut self) -> Vec3 {
        self.unit_vector() * self.next_f32().cbrt()
    }

    /// 单位圆盘（XZ 平面）内部均匀分布的一点。半径开平方，理由同上。
    pub fn in_unit_disk(&mut self) -> Vec3 {
        let angle = self.next_f32() * std::f32::consts::TAU;
        let radius = self.next_f32().sqrt();
        Vec3::new(radius * angle.cos(), 0.0, radius * angle.sin())
    }

    /// 单位立方体内部均匀分布的一点，各分量取 `[-1, 1)`。
    pub fn in_unit_cube(&mut self) -> Vec3 {
        Vec3::new(self.next_signed(), self.next_signed(), self.next_signed())
    }

    /// 以 `axis` 为轴、半角为 `spread` 弧度的圆锥内的随机方向。
    ///
    /// `spread` 为 0 时退化成 `axis` 本身，为 π 时退化成全向。
    pub fn in_cone(&mut self, axis: Vec3, spread: f32) -> Vec3 {
        let axis = axis.normalize_or_zero();
        if axis == Vec3::ZERO {
            return self.unit_vector();
        }
        let spread = spread.clamp(0.0, std::f32::consts::PI);
        // cos θ 在 [cos spread, 1] 上均匀 —— 这才是锥面上的均匀分布。
        let cos_theta = 1.0 - self.next_f32() * (1.0 - spread.cos());
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let phi = self.next_f32() * std::f32::consts::TAU;

        // 在轴周围建一组正交基。取与轴最不平行的坐标轴做参考，避免叉积退化。
        let reference = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        let tangent = axis.cross(reference).normalize();
        let bitangent = axis.cross(tangent);

        (tangent * (sin_theta * phi.cos()) + bitangent * (sin_theta * phi.sin()) + axis * cos_theta)
            .normalize_or_zero()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn same_seed_gives_same_sequence() {
        // 确定性是粒子可测试的前提：同种子必须逐位相同。
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);

        for _ in 0..64 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);

        assert_ne!(
            (0..8).map(|_| a.next_u32()).collect::<Vec<_>>(),
            (0..8).map(|_| b.next_u32()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn reset_replays_the_same_sequence() {
        let mut rng = Rng::new(7);
        let first: Vec<u32> = (0..16).map(|_| rng.next_u32()).collect();

        rng.reset();

        assert_eq!(first, (0..16).map(|_| rng.next_u32()).collect::<Vec<_>>());
    }

    #[test]
    fn floats_stay_in_unit_range() {
        let mut rng = Rng::new(3);

        for _ in 0..10_000 {
            let value = rng.next_f32();
            assert!((0.0..1.0).contains(&value), "{value} 越界");
        }
    }

    #[test]
    fn floats_are_roughly_uniform() {
        let mut rng = Rng::new(11);
        let mut buckets = [0u32; 10];

        for _ in 0..100_000 {
            buckets[(rng.next_f32() * 10.0) as usize % 10] += 1;
        }

        // 每桶期望 10000，偏差超过 15% 说明发生器有问题。
        for (index, count) in buckets.iter().enumerate() {
            assert!(
                (8_500..11_500).contains(count),
                "第 {index} 个桶有 {count} 个样本，分布不均"
            );
        }
    }

    #[test]
    fn unit_vectors_are_normalized() {
        let mut rng = Rng::new(5);

        for _ in 0..1000 {
            assert!((rng.unit_vector().length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn unit_vectors_do_not_cluster_at_the_poles() {
        let mut rng = Rng::new(9);
        // 均匀球面分布下，|z| < 0.5 的比例应当正好是一半。
        let near_equator = (0..10_000)
            .filter(|_| rng.unit_vector().z.abs() < 0.5)
            .count();

        assert!(
            (4_500..5_500).contains(&near_equator),
            "赤道带比例 {near_equator}/10000，方向分布不均匀"
        );
    }

    #[test]
    fn sphere_samples_stay_inside() {
        let mut rng = Rng::new(13);

        for _ in 0..1000 {
            assert!(rng.in_unit_sphere().length() <= 1.0 + 1e-5);
        }
    }

    #[test]
    fn sphere_samples_are_volume_uniform() {
        let mut rng = Rng::new(17);
        // 半径小于 1/2 的球只占总体积的 1/8，样本比例应当接近 12.5%。
        let inner = (0..20_000)
            .filter(|_| rng.in_unit_sphere().length() < 0.5)
            .count();

        assert!(
            (2_000..3_000).contains(&inner),
            "内层占比 {inner}/20000，半径没有按体积加权"
        );
    }

    #[test]
    fn disk_samples_lie_flat_and_inside() {
        let mut rng = Rng::new(19);

        for _ in 0..1000 {
            let point = rng.in_unit_disk();
            assert_eq!(point.y, 0.0);
            assert!(point.length() <= 1.0 + 1e-5);
        }
    }

    #[test]
    fn cone_respects_its_half_angle() {
        let mut rng = Rng::new(23);
        let axis = Vec3::Y;
        let spread = 0.3;

        for _ in 0..2000 {
            let direction = rng.in_cone(axis, spread);
            let angle = direction.dot(axis).clamp(-1.0, 1.0).acos();
            assert!(angle <= spread + 1e-4, "偏离轴 {angle} 弧度，超出锥角");
        }
    }

    #[test]
    fn zero_spread_cone_points_straight_along_the_axis() {
        let mut rng = Rng::new(29);

        let direction = rng.in_cone(Vec3::new(0.0, 2.0, 0.0), 0.0);

        assert!((direction - Vec3::Y).length() < 1e-5);
    }

    #[test]
    fn degenerate_cone_axis_still_gives_a_direction() {
        let mut rng = Rng::new(31);

        // 轴为零向量时不能给出 NaN，否则粒子会整体消失。
        let direction = rng.in_cone(Vec3::ZERO, 0.5);

        assert!(direction.is_finite());
        assert!((direction.length() - 1.0).abs() < 1e-5);
    }
}
