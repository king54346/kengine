//! 程序化天空。
//!
//! 不加载 HDR 环境贴图，而是用一个解析函数描述天空——
//! 好处是环境光可以在 CPU 上积分出来（见 [`crate::ibl`]），无需 GPU 预计算管线，
//! 而且这份函数在着色器里也有等价实现，两边采样结果一致。

use kmath::Vec3;

/// 一片渐变天空 + 一轮太阳。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sky {
    /// 天顶颜色。
    pub zenith: Vec3,
    /// 地平线颜色。
    pub horizon: Vec3,
    /// 地面（向下方向）颜色。
    pub ground: Vec3,
    /// 指向太阳的方向（单位向量）。
    pub sun_direction: Vec3,
    /// 太阳颜色，可大于 1 表示高光强度。
    pub sun_color: Vec3,
    /// 太阳角半径的余弦阈值，越接近 1 太阳越小。
    pub sun_size: f32,
}

impl Default for Sky {
    fn default() -> Self {
        Self {
            zenith: Vec3::new(0.18, 0.34, 0.72),
            horizon: Vec3::new(0.62, 0.72, 0.86),
            ground: Vec3::new(0.16, 0.15, 0.14),
            sun_direction: Vec3::new(0.45, 0.62, 0.35).normalize(),
            sun_color: Vec3::new(8.0, 7.2, 6.0),
            sun_size: 0.9995,
        }
    }
}

impl kcore::visitor::Visit for Sky {
    fn visit(
        &mut self,
        name: &str,
        visitor: &mut kcore::visitor::Visitor,
    ) -> kcore::visitor::VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.zenith.visit("Zenith", &mut region)?;
        self.horizon.visit("Horizon", &mut region)?;
        self.ground.visit("Ground", &mut region)?;
        self.sun_direction.visit("SunDirection", &mut region)?;
        self.sun_color.visit("SunColor", &mut region)?;
        self.sun_size.visit("SunSize", &mut region)?;
        Ok(())
    }
}

impl Sky {
    /// 采样某个方向上的天空辐射亮度。
    ///
    /// `direction` 需已归一化。
    pub fn sample(&self, direction: Vec3) -> Vec3 {
        let up = direction.y;

        // 地平线以上：地平线色 → 天顶色；以下：地平线色 → 地面色。
        // 用 sqrt 让过渡集中在地平线附近，更接近真实天空的观感。
        let base = if up >= 0.0 {
            self.horizon.lerp(self.zenith, up.sqrt())
        } else {
            self.horizon.lerp(self.ground, (-up).sqrt())
        };

        // 太阳：方向足够接近时叠加一个亮斑。
        let cos_angle = direction.dot(self.sun_direction);
        if cos_angle > self.sun_size {
            // 边缘平滑过渡，避免锯齿状的硬边。
            let t = ((cos_angle - self.sun_size) / (1.0 - self.sun_size).max(1e-6)).clamp(0.0, 1.0);
            base + self.sun_color * t * t
        } else {
            base
        }
    }

    /// 不含太阳的天空，用于积分环境光时避免太阳被重复计入。
    ///
    /// 太阳通常已经用一盏方向光单独表示，环境光里再算一次会导致过曝。
    pub fn sample_without_sun(&self, direction: Vec3) -> Vec3 {
        let up = direction.y;
        if up >= 0.0 {
            self.horizon.lerp(self.zenith, up.sqrt())
        } else {
            self.horizon.lerp(self.ground, (-up).sqrt())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn zenith_and_ground_match_configuration() {
        let sky = Sky {
            sun_color: Vec3::ZERO,
            ..Default::default()
        };

        assert!((sky.sample(Vec3::Y) - sky.zenith).length() < 1e-5);
        assert!((sky.sample(-Vec3::Y) - sky.ground).length() < 1e-5);
    }

    #[test]
    fn horizon_is_reached_at_equator() {
        let sky = Sky::default();

        let value = sky.sample_without_sun(Vec3::new(1.0, 0.0, 0.0));

        assert!((value - sky.horizon).length() < 1e-5);
    }

    #[test]
    fn sun_adds_energy_in_its_direction() {
        let sky = Sky::default();

        let with_sun = sky.sample(sky.sun_direction);
        let without_sun = sky.sample_without_sun(sky.sun_direction);

        assert!(with_sun.length() > without_sun.length());
    }

    #[test]
    fn sun_does_not_leak_far_from_its_direction() {
        let sky = Sky::default();
        // 与太阳相反的方向不该有任何太阳贡献。
        let opposite = -sky.sun_direction;

        assert!(
            (sky.sample(opposite) - sky.sample_without_sun(opposite)).length() < 1e-6
        );
    }

    #[test]
    fn output_is_always_finite_and_non_negative() {
        let sky = Sky::default();

        for i in 0..64 {
            let t = i as f32 / 64.0 * std::f32::consts::TAU;
            for y in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                let direction = Vec3::new(t.cos(), y, t.sin()).normalize();
                let value = sky.sample(direction);

                assert!(value.is_finite(), "方向 {direction:?} 采样出 NaN");
                assert!(value.min_element() >= 0.0, "天空不应出现负亮度");
            }
        }
    }

    #[test]
    fn degenerate_sun_size_does_not_divide_by_zero() {
        // sun_size = 1 时分母为 0，必须有保护。
        let sky = Sky {
            sun_size: 1.0,
            ..Default::default()
        };

        assert!(sky.sample(sky.sun_direction).is_finite());
    }
}
