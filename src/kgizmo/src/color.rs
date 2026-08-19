//! 调试线的颜色。
//!
//! 单独立一个类型而不是直接用 `Vec4`，是为了两件事：一是让 `Color::RED`
//! 这样的常量有地方放；二是 rapier 的调试渲染回调给的是 **HSLA**，
//! 需要一个明确的入口做转换，而不是在调用点手写一遍色彩空间公式。

use bytemuck::{Pod, Zeroable};

/// 线性空间的 RGBA 颜色。
///
/// **是线性值，不是 sRGB。** 调试线画在 HDR 目标上，之后还要过一遍色调映射，
/// 所以这里存的必须是线性值；填 sRGB 的数字会在屏幕上偏亮。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Color {
    /// 红。
    pub r: f32,
    /// 绿。
    pub g: f32,
    /// 蓝。
    pub b: f32,
    /// 不透明度。
    pub a: f32,
}

impl Color {
    /// 不透明的黑。
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);
    /// 不透明的白。
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);
    /// 红。
    pub const RED: Self = Self::rgb(1.0, 0.0, 0.0);
    /// 绿。
    pub const GREEN: Self = Self::rgb(0.0, 1.0, 0.0);
    /// 蓝。
    pub const BLUE: Self = Self::rgb(0.0, 0.3, 1.0);
    /// 黄。
    pub const YELLOW: Self = Self::rgb(1.0, 1.0, 0.0);
    /// 青。
    pub const CYAN: Self = Self::rgb(0.0, 1.0, 1.0);
    /// 品红。
    pub const MAGENTA: Self = Self::rgb(1.0, 0.0, 1.0);
    /// 橙。
    pub const ORANGE: Self = Self::rgb(1.0, 0.45, 0.0);
    /// 灰。
    pub const GRAY: Self = Self::rgb(0.5, 0.5, 0.5);

    /// 不透明颜色。
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// 带不透明度的颜色。
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// 只改不透明度，颜色不变。
    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// 整体调亮/调暗（乘在 RGB 上，不动 alpha）。
    pub fn scaled(self, factor: f32) -> Self {
        Self {
            r: self.r * factor,
            g: self.g * factor,
            b: self.b * factor,
            a: self.a,
        }
    }

    /// 由 HSLA 构造：`h` 取 `0..=360` 度，`s`/`l`/`a` 取 `0..=1`。
    ///
    /// rapier 的调试渲染就是按这个色彩空间给颜色的，所以这条路径必须有。
    pub fn from_hsla(h: f32, s: f32, l: f32, a: f32) -> Self {
        // 色相是周期量，先归到 [0, 360) 再算，负数和超出一圈的输入都能接住。
        let h = h.rem_euclid(360.0);
        let s = s.clamp(0.0, 1.0);
        let l = l.clamp(0.0, 1.0);

        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = l - c * 0.5;

        let (r, g, b) = match h as u32 / 60 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            // 300..360，以及 h 正好等于 360 时 `as u32 / 60 == 6` 的边界。
            _ => (c, 0.0, x),
        };

        Self {
            r: r + m,
            g: g + m,
            b: b + m,
            a: a.clamp(0.0, 1.0),
        }
    }

    /// 展开成数组，方便塞进顶点。
    pub const fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

impl From<[f32; 4]> for Color {
    fn from([r, g, b, a]: [f32; 4]) -> Self {
        Self { r, g, b, a }
    }
}

impl From<Color> for [f32; 4] {
    fn from(c: Color) -> Self {
        c.to_array()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Color, b: Color) -> bool {
        (a.r - b.r).abs() < 1e-5
            && (a.g - b.g).abs() < 1e-5
            && (a.b - b.b).abs() < 1e-5
            && (a.a - b.a).abs() < 1e-5
    }

    #[test]
    fn hsla_hits_the_primaries() {
        assert!(close(Color::from_hsla(0.0, 1.0, 0.5, 1.0), Color::RED));
        assert!(close(Color::from_hsla(120.0, 1.0, 0.5, 1.0), Color::GREEN));
        assert!(close(
            Color::from_hsla(240.0, 1.0, 0.5, 1.0),
            Color::rgb(0.0, 0.0, 1.0)
        ));
    }

    #[test]
    fn hsla_zero_saturation_is_gray() {
        let c = Color::from_hsla(217.0, 0.0, 0.25, 1.0);
        assert!(close(c, Color::rgb(0.25, 0.25, 0.25)));
    }

    #[test]
    fn hsla_wraps_the_hue() {
        // 色相是周期量：-60 与 300 必须是同一个颜色，420 与 60 也是。
        assert!(close(
            Color::from_hsla(-60.0, 1.0, 0.5, 1.0),
            Color::from_hsla(300.0, 1.0, 0.5, 1.0)
        ));
        assert!(close(
            Color::from_hsla(420.0, 0.8, 0.4, 1.0),
            Color::from_hsla(60.0, 0.8, 0.4, 1.0)
        ));
    }

    #[test]
    fn hsla_at_exactly_360_is_not_a_hole() {
        // 360 度落在分段函数的边界上，早期实现在这里会掉进 `_` 分支给出错的颜色。
        assert!(close(Color::from_hsla(360.0, 1.0, 0.5, 1.0), Color::RED));
    }

    #[test]
    fn hsla_extremes_are_black_and_white() {
        assert!(close(Color::from_hsla(200.0, 1.0, 0.0, 1.0), Color::BLACK));
        assert!(close(Color::from_hsla(200.0, 1.0, 1.0, 1.0), Color::WHITE));
    }
}
