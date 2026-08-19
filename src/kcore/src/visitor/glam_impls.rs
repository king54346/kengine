//! glam 数学类型的序列化。
//!
//! `impls.rs` 里那套是 nalgebra 版的（随 Visitor 一起从 Fyrox 移植过来），
//! 而本项目其余部分用的是 glam。孤儿规则下 `impl Visit for glam::Vec3`
//! 只能写在**拥有 `Visit` 的这个 crate**里，所以它落在 kcore 而不是 kmath。
//!
//! 存法是**逐分量的命名字段**，不是内存直接倾倒。多花几个字节换来的是：
//! ASCII 格式下人能读懂、两端按名字对齐（不怕将来加字段）、
//! 也不依赖 glam 的内部布局（`Vec3A`、`Mat3A` 都是带填充的）。

use crate::visitor::{Visit, VisitResult, Visitor};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};

impl Visit for Vec2 {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.x.visit("X", &mut region)?;
        self.y.visit("Y", &mut region)?;
        Ok(())
    }
}

impl Visit for Vec3 {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.x.visit("X", &mut region)?;
        self.y.visit("Y", &mut region)?;
        self.z.visit("Z", &mut region)?;
        Ok(())
    }
}

impl Visit for Vec4 {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.x.visit("X", &mut region)?;
        self.y.visit("Y", &mut region)?;
        self.z.visit("Z", &mut region)?;
        self.w.visit("W", &mut region)?;
        Ok(())
    }
}

impl Visit for Quat {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let (mut x, mut y, mut z, mut w) = (self.x, self.y, self.z, self.w);
        x.visit("X", &mut region)?;
        y.visit("Y", &mut region)?;
        z.visit("Z", &mut region)?;
        w.visit("W", &mut region)?;

        if region.is_reading() {
            let restored = Quat::from_xyzw(x, y, z, w);
            // 四元数必须是单位长度，否则拿去做旋转会顺带缩放。
            // 文件被手改过、或是历史遗留的零四元数，都在这里兜住。
            *self = if restored.length_squared() > 1e-12 {
                restored.normalize()
            } else {
                Quat::IDENTITY
            };
        }

        Ok(())
    }
}

impl Visit for Mat3 {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.x_axis.visit("X", &mut region)?;
        self.y_axis.visit("Y", &mut region)?;
        self.z_axis.visit("Z", &mut region)?;
        Ok(())
    }
}

impl Visit for Mat4 {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.x_axis.visit("X", &mut region)?;
        self.y_axis.visit("Y", &mut region)?;
        self.z_axis.visit("Z", &mut region)?;
        self.w_axis.visit("W", &mut region)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// 存进 Visitor 再读回来。
    fn roundtrip<T: Visit + Default + Copy>(value: &T) -> T {
        let mut visitor = Visitor::new();
        let mut source = *value;
        source.visit("V", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = T::default();
        restored.visit("V", &mut visitor).unwrap();
        restored
    }

    #[test]
    fn vectors_survive_a_roundtrip() {
        assert_eq!(roundtrip(&Vec2::new(1.5, -2.5)), Vec2::new(1.5, -2.5));
        assert_eq!(
            roundtrip(&Vec3::new(1.5, -2.5, 3.25)),
            Vec3::new(1.5, -2.5, 3.25)
        );
        assert_eq!(
            roundtrip(&Vec4::new(1.5, -2.5, 3.25, 4.0)),
            Vec4::new(1.5, -2.5, 3.25, 4.0)
        );
    }

    #[test]
    fn a_quaternion_survives_and_stays_normalised() {
        let q = Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 0.9);
        let restored = roundtrip(&q);

        assert!((restored.length() - 1.0).abs() < 1e-6);
        // 真正要保住的是它描述的旋转。
        let v = Vec3::new(0.3, -0.7, 2.0);
        assert!((restored * v - q * v).length() < 1e-5);
    }

    #[test]
    fn a_degenerate_quaternion_reads_back_as_identity() {
        // 手改过的文件、或历史遗留的全零四元数，不该产出一个会把模型压扁的旋转。
        let mut visitor = Visitor::new();
        let (mut x, mut y, mut z, mut w) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        {
            let mut region = visitor.enter_region("V").unwrap();
            x.visit("X", &mut region).unwrap();
            y.visit("Y", &mut region).unwrap();
            z.visit("Z", &mut region).unwrap();
            w.visit("W", &mut region).unwrap();
        }
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = Quat::IDENTITY;
        restored.visit("V", &mut visitor).unwrap();

        assert_eq!(restored, Quat::IDENTITY);
    }

    #[test]
    fn matrices_survive_a_roundtrip() {
        let m = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 2.0, 3.0),
            Quat::from_rotation_y(0.4),
            Vec3::new(4.0, 5.0, 6.0),
        );
        assert_eq!(roundtrip(&m), m);

        let m3 = Mat3::from_rotation_z(0.7);
        assert_eq!(roundtrip(&m3), m3);
    }

    #[test]
    fn the_ascii_dump_is_human_readable() {
        // 逐分量的命名字段换来的好处：出问题时能直接看文件。
        let mut visitor = Visitor::new();
        let mut v = Vec3::new(1.0, 2.0, 3.0);
        v.visit("Position", &mut visitor).unwrap();

        let text = visitor.save_ascii_to_string();
        assert!(text.contains("Position"), "{text}");
        assert!(
            text.contains('X') && text.contains('Y') && text.contains('Z'),
            "{text}"
        );
    }
}
