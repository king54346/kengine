//! kmath 自有几何类型的序列化。
//!
//! glam 类型的实现在 `kcore::visitor::glam_impls` —— 孤儿规则要求
//! `impl Visit for glam::Vec3` 写在拥有 `Visit` 的 kcore 里。
//! [`Aabb`] 与 [`Plane`] 是 kmath 自己的类型，实现落在这边。

use crate::{Aabb, Plane};
use kcore::visitor::{Visit, VisitResult, Visitor};

impl Visit for Aabb {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.min.visit("Min", &mut region)?;
        self.max.visit("Max", &mut region)?;
        Ok(())
    }
}

impl Visit for Plane {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.normal.visit("Normal", &mut region)?;
        self.d.visit("D", &mut region)?;
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
    fn an_aabb_survives_a_roundtrip() {
        let aabb = Aabb::new(Vec3::new(-1.0, -2.0, -3.0), Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(roundtrip(&aabb), aabb);
    }

    #[test]
    fn a_plane_survives_a_roundtrip() {
        // `Plane` 没有 `Default`（零法线的平面没有意义），手工往返一趟。
        let plane = Plane {
            normal: Vec3::Y,
            d: 2.5,
        };
        let mut visitor = Visitor::new();
        let mut source = plane;
        source.visit("V", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = Plane {
            normal: Vec3::X,
            d: 0.0,
        };
        restored.visit("V", &mut visitor).unwrap();

        assert!((restored.normal - plane.normal).length() < 1e-6);
        assert!((restored.d - plane.d).abs() < 1e-6);
    }

}
