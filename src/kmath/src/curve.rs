//! 三次曲线：Bezier、Hermite、Cardinal（含 Catmull-Rom）、B 样条。
//!
//! 相机轨道、巡逻路径、缓动函数、样条地形——凡是「给几个点，要一条顺滑地
//! 穿过去的线」的地方都用它。
//!
//! # 为什么是三次
//!
//! 三次多项式是**位置、速度都能同时对齐**的最低次数：两端各有位置和切线
//! 四个约束，正好定死四个系数。二次做不到（只有三个系数，接缝处的速度会跳），
//! 更高次则会在控制点之间来回摆动（龙格现象），反而不像手画的线。
//!
//! # 四种曲线怎么选
//!
//! | | 过不过控制点 | 要额外输入 | 典型用途 |
//! |---|---|---|---|
//! | [`CubicBezier`] | 过首尾，不过中间两个 | 每段四个点 | 字体轮廓、缓动、美术工具里拖手柄 |
//! | [`CubicHermite`] | **全都过** | 每点一个切线 | 已知速度的轨迹（导出的相机运动） |
//! | [`CubicCardinalSpline`] | **全都过** | 一个松紧参数 | 巡逻路径：只给点，切线自动推 |
//! | [`CubicBSpline`] | **一个都不过** | — | 要求最顺滑、控制点只当「吸引子」 |
//!
//! 拿不准就用 [`CubicCardinalSpline::catmull_rom`]：给一串点就能得到一条
//! 穿过它们的顺滑曲线，不必额外提供切线。
//!
//! # 怎么算的
//!
//! 每一段都是四个控制点的线性组合，组合系数由一个 4×4 的**特征矩阵**决定：
//!
//! ```text
//! c = M · P                        （c 是四个多项式系数，P 是四个控制点）
//! position(t) = c₀ + c₁t + c₂t² + c₃t³
//! ```
//!
//! 四种曲线的区别**只在那个矩阵**。所以求值代码只有一份，构造器各自把
//! 控制点排好、乘上自己的矩阵。

use crate::{Vec2, Vec3, Vec4};
use std::ops::{Add, Mul, Sub};

/// 能当曲线取值的类型：能加、能减、能乘一个标量。
///
/// [`Vec2`]、[`Vec3`]、[`Vec4`] 和 [`f32`] 都满足。`f32` 那份是给**缓动曲线**
/// 用的——一条一维的三次曲线就是一条 ease-in-out。
pub trait Point:
    Copy + Add<Self, Output = Self> + Sub<Self, Output = Self> + Mul<f32, Output = Self>
{
    /// 零值。构造空曲线时兜底用。
    const ZERO: Self;
}

impl Point for f32 {
    const ZERO: Self = 0.0;
}
impl Point for Vec2 {
    const ZERO: Self = Vec2::ZERO;
}
impl Point for Vec3 {
    const ZERO: Self = Vec3::ZERO;
}
impl Point for Vec4 {
    const ZERO: Self = Vec4::ZERO;
}

/// 构造曲线时点数不够。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotEnoughPoints {
    /// 至少要几个。
    pub needed: usize,
    /// 实际给了几个。
    pub given: usize,
}

impl std::fmt::Display for NotEnoughPoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "控制点不够：至少要 {} 个，只给了 {}",
            self.needed, self.given
        )
    }
}

impl std::error::Error for NotEnoughPoints {}

/// 一段三次曲线，由四个多项式系数决定。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicSegment<P: Point> {
    /// `c₀ + c₁t + c₂t² + c₃t³` 的四个系数。
    coefficients: [P; 4],
}

impl<P: Point> CubicSegment<P> {
    /// 由四个控制点与一个特征矩阵造一段。
    ///
    /// 矩阵按行给：`c[i] = Σⱼ matrix[i][j] · points[j]`。
    fn from_matrix(points: [P; 4], matrix: [[f32; 4]; 4]) -> Self {
        let combine = |row: [f32; 4]| {
            points[0] * row[0] + points[1] * row[1] + points[2] * row[2] + points[3] * row[3]
        };
        Self {
            coefficients: [
                combine(matrix[0]),
                combine(matrix[1]),
                combine(matrix[2]),
                combine(matrix[3]),
            ],
        }
    }

    /// `t ∈ [0, 1]` 处的位置。
    ///
    /// 用 Horner 展开（`c₀ + t(c₁ + t(c₂ + t·c₃))`）而不是直接算 `t²`、`t³`：
    /// 少两次乘法，数值上也更稳。
    pub fn position(&self, t: f32) -> P {
        let [c0, c1, c2, c3] = self.coefficients;
        c0 + (c1 + (c2 + c3 * t) * t) * t
    }

    /// `t` 处的一阶导数（速度）。
    pub fn velocity(&self, t: f32) -> P {
        let [_, c1, c2, c3] = self.coefficients;
        c1 + (c2 * 2.0 + c3 * (3.0 * t)) * t
    }

    /// `t` 处的二阶导数（加速度）。
    pub fn acceleration(&self, t: f32) -> P {
        let [_, _, c2, c3] = self.coefficients;
        c2 * 2.0 + c3 * (6.0 * t)
    }
}

/// 一串首尾相接的三次段。
///
/// 参数域是 `[0, 段数]`：`position(0.0)` 是起点，`position(1.5)` 是第二段的
/// 中点。这样加一段不会把已有那些点的参数全挪一遍——按弧长或按 `[0,1]`
/// 归一化都会有那个毛病。
#[derive(Debug, Clone, PartialEq)]
pub struct CubicCurve<P: Point> {
    segments: Vec<CubicSegment<P>>,
}

impl<P: Point> CubicCurve<P> {
    /// 全部段。
    pub fn segments(&self) -> &[CubicSegment<P>] {
        &self.segments
    }

    /// 一段都没有。
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// 参数域的右端，也就是段数。
    pub fn domain_end(&self) -> f32 {
        self.segments.len() as f32
    }

    /// 把参数拆成「第几段」与「段内的 t」。
    ///
    /// 超出两端时**夹住**而不是外推：三次多项式在定义域外会迅速跑飞，
    /// 一条本来老实的路径在 `t = 1.01` 处可能已经飞出屏幕。
    fn locate(&self, t: f32) -> (&CubicSegment<P>, f32) {
        let last = self.segments.len().saturating_sub(1);
        if t <= 0.0 {
            return (&self.segments[0], 0.0);
        }
        if t >= self.domain_end() {
            return (&self.segments[last], 1.0);
        }
        let index = t.floor() as usize;
        (&self.segments[index.min(last)], t - index as f32)
    }

    /// `t ∈ [0, 段数]` 处的位置。空曲线返回零值。
    pub fn position(&self, t: f32) -> P {
        if self.segments.is_empty() {
            return P::ZERO;
        }
        let (segment, local) = self.locate(t);
        segment.position(local)
    }

    /// `t` 处的速度。空曲线返回零值。
    pub fn velocity(&self, t: f32) -> P {
        if self.segments.is_empty() {
            return P::ZERO;
        }
        let (segment, local) = self.locate(t);
        segment.velocity(local)
    }

    /// `t` 处的加速度。空曲线返回零值。
    pub fn acceleration(&self, t: f32) -> P {
        if self.segments.is_empty() {
            return P::ZERO;
        }
        let (segment, local) = self.locate(t);
        segment.acceleration(local)
    }

    /// 沿曲线均匀取 `count` 个点，含两端。
    ///
    /// 画曲线就是把它拆成足够多的线段。`count` 要**随段数一起涨**，
    /// 否则曲线越长看着越折——固定取 100 个点的话，二十段的曲线每段只剩五个。
    pub fn iter_positions(&self, count: usize) -> impl Iterator<Item = P> + '_ {
        let end = self.domain_end();
        let last = count.saturating_sub(1).max(1) as f32;
        (0..count).map(move |i| self.position(i as f32 / last * end))
    }
}

// ── 特征矩阵 ──────────────────────────────────────────────────────────────────
//
// 四种曲线的区别全在这四张表里。行是多项式的次数（常数项、一次、二次、三次），
// 列是四个控制点的权重。

/// Bezier：`P₀` 与 `P₃` 是端点，`P₁`、`P₂` 是拉扯形状的手柄。
const BEZIER: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [-3.0, 3.0, 0.0, 0.0],
    [3.0, -6.0, 3.0, 0.0],
    [-1.0, 3.0, -3.0, 1.0],
];

/// Hermite：控制点排成 `[p₀, v₀, p₁, v₁]`——位置和切线交替。
const HERMITE: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [-3.0, -2.0, 3.0, -1.0],
    [2.0, 1.0, -2.0, 1.0],
];

/// B 样条：曲线**不经过**任何控制点，换来的是二阶连续（加速度也不跳）。
const B_SPLINE: [[f32; 4]; 4] = [
    [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0, 0.0],
    [-3.0 / 6.0, 0.0, 3.0 / 6.0, 0.0],
    [3.0 / 6.0, -6.0 / 6.0, 3.0 / 6.0, 0.0],
    [-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0],
];

/// Cardinal 的矩阵跟松紧参数走，所以是算出来的而不是常量。
///
/// 段画在 `p₁ → p₂` 之间，`p₀`、`p₃` 只用来推两端的切线：
/// `v₁ = s(p₂ - p₀)`，`v₂ = s(p₃ - p₁)`。
fn cardinal_matrix(tension: f32) -> [[f32; 4]; 4] {
    let s = tension;
    [
        [0.0, 1.0, 0.0, 0.0],
        [-s, 0.0, s, 0.0],
        [2.0 * s, s - 3.0, 3.0 - 2.0 * s, -s],
        [-s, 2.0 - s, s - 2.0, s],
    ]
}

// ── 构造器 ────────────────────────────────────────────────────────────────────

/// 逐段给四个控制点的贝塞尔曲线。
///
/// 曲线过每段的首尾控制点，中间两个是把形状拉过去的手柄——美术工具里
/// 拖的就是它们。
#[derive(Debug, Clone)]
pub struct CubicBezier<P: Point> {
    /// 每段四个控制点。
    pub segments: Vec<[P; 4]>,
}

impl<P: Point> CubicBezier<P> {
    /// 由若干段控制点构造。
    pub fn new(segments: impl Into<Vec<[P; 4]>>) -> Self {
        Self {
            segments: segments.into(),
        }
    }

    /// 生成曲线。一段都没有时报错。
    pub fn to_curve(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        if self.segments.is_empty() {
            return Err(NotEnoughPoints {
                needed: 1,
                given: 0,
            });
        }
        Ok(CubicCurve {
            segments: self
                .segments
                .iter()
                .map(|points| CubicSegment::from_matrix(*points, BEZIER))
                .collect(),
        })
    }
}

/// 每个控制点都带一条切线的曲线。
///
/// 曲线**经过每一个控制点**，而且在那里的速度正好是你给的切线。
/// 已知速度的轨迹（比如从别处导出的相机运动）用它才不会走样。
#[derive(Debug, Clone)]
pub struct CubicHermite<P: Point> {
    /// 控制点。
    pub points: Vec<P>,
    /// 每个控制点处的切线，与 `points` 一一对应。
    pub tangents: Vec<P>,
}

impl<P: Point> CubicHermite<P> {
    /// 由点与切线构造。两者长度不等时按短的算。
    pub fn new(points: impl Into<Vec<P>>, tangents: impl Into<Vec<P>>) -> Self {
        Self {
            points: points.into(),
            tangents: tangents.into(),
        }
    }

    fn usable(&self) -> usize {
        self.points.len().min(self.tangents.len())
    }

    /// 生成开曲线：`n` 个点连成 `n-1` 段。
    pub fn to_curve(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        let n = self.usable();
        if n < 2 {
            return Err(NotEnoughPoints {
                needed: 2,
                given: n,
            });
        }
        Ok(self.build(n - 1, false))
    }

    /// 生成闭曲线：多出一段从最后一个点绕回第一个点，`n` 个点得 `n` 段。
    pub fn to_curve_cyclic(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        let n = self.usable();
        if n < 2 {
            return Err(NotEnoughPoints {
                needed: 2,
                given: n,
            });
        }
        Ok(self.build(n, true))
    }

    fn build(&self, count: usize, cyclic: bool) -> CubicCurve<P> {
        let n = self.usable();
        let segments = (0..count)
            .map(|i| {
                let next = if cyclic { (i + 1) % n } else { i + 1 };
                CubicSegment::from_matrix(
                    [
                        self.points[i],
                        self.tangents[i],
                        self.points[next],
                        self.tangents[next],
                    ],
                    HERMITE,
                )
            })
            .collect();
        CubicCurve { segments }
    }
}

/// 只给点、切线自动推出来的曲线。
///
/// 每个控制点处的切线取自它前后两个邻居：`vᵢ = s(pᵢ₊₁ - pᵢ₋₁)`。
/// 曲线**经过每一个控制点**，而你只需要给点——巡逻路径、相机轨道最省事的一种。
#[derive(Debug, Clone)]
pub struct CubicCardinalSpline<P: Point> {
    /// 控制点。
    pub points: Vec<P>,
    /// 松紧。0 是折线一样绷直，值越大拐弯处越鼓。
    ///
    /// 0.5 就是 **Catmull-Rom**，见 [`catmull_rom`](Self::catmull_rom)。
    /// 超过 1 之后曲线会在控制点附近打圈（过冲），通常不是想要的。
    pub tension: f32,
}

impl<P: Point> CubicCardinalSpline<P> {
    /// 指定松紧。
    pub fn new(tension: f32, points: impl Into<Vec<P>>) -> Self {
        Self {
            points: points.into(),
            tension,
        }
    }

    /// Catmull-Rom：`tension = 0.5`。
    ///
    /// 拿不准用哪种曲线时用这个——给一串点就得到一条穿过它们的顺滑线。
    pub fn catmull_rom(points: impl Into<Vec<P>>) -> Self {
        Self::new(0.5, points)
    }

    /// 生成开曲线。
    ///
    /// 首尾两段缺一个邻居，这里把端点自己**当作那个邻居**（等价于在两端
    /// 各接一个重合点）。另一种常见做法是镜像外推，那样端点处的弯会更饱满，
    /// 但也更容易冲出控制点围成的范围。
    pub fn to_curve(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        let n = self.points.len();
        if n < 2 {
            return Err(NotEnoughPoints {
                needed: 2,
                given: n,
            });
        }

        let matrix = cardinal_matrix(self.tension);
        let segments = (0..n - 1)
            .map(|i| {
                let previous = self.points[i.saturating_sub(1)];
                let after = self.points[(i + 2).min(n - 1)];
                CubicSegment::from_matrix(
                    [previous, self.points[i], self.points[i + 1], after],
                    matrix,
                )
            })
            .collect();
        Ok(CubicCurve { segments })
    }

    /// 生成闭曲线：邻居按环取，`n` 个点得 `n` 段。
    pub fn to_curve_cyclic(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        let n = self.points.len();
        if n < 3 {
            return Err(NotEnoughPoints {
                needed: 3,
                given: n,
            });
        }

        let matrix = cardinal_matrix(self.tension);
        let segments = (0..n)
            .map(|i| {
                CubicSegment::from_matrix(
                    [
                        self.points[(i + n - 1) % n],
                        self.points[i],
                        self.points[(i + 1) % n],
                        self.points[(i + 2) % n],
                    ],
                    matrix,
                )
            })
            .collect();
        Ok(CubicCurve { segments })
    }
}

/// 均匀三次 B 样条。
///
/// 曲线**一个控制点都不经过**，它们只是把线拉过去的吸引子。换来的是
/// 二阶连续——不只速度不跳，加速度也不跳。相机运动用它最不容易让人晕。
#[derive(Debug, Clone)]
pub struct CubicBSpline<P: Point> {
    /// 控制点。
    pub points: Vec<P>,
}

impl<P: Point> CubicBSpline<P> {
    /// 由控制点构造。
    pub fn new(points: impl Into<Vec<P>>) -> Self {
        Self {
            points: points.into(),
        }
    }

    /// 生成开曲线：每四个连续控制点出一段，`n` 个点得 `n-3` 段。
    pub fn to_curve(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        let n = self.points.len();
        if n < 4 {
            return Err(NotEnoughPoints {
                needed: 4,
                given: n,
            });
        }
        let segments = (0..n - 3)
            .map(|i| {
                CubicSegment::from_matrix(
                    [
                        self.points[i],
                        self.points[i + 1],
                        self.points[i + 2],
                        self.points[i + 3],
                    ],
                    B_SPLINE,
                )
            })
            .collect();
        Ok(CubicCurve { segments })
    }

    /// 生成闭曲线：控制点按环取，`n` 个点得 `n` 段。
    pub fn to_curve_cyclic(&self) -> Result<CubicCurve<P>, NotEnoughPoints> {
        let n = self.points.len();
        if n < 3 {
            return Err(NotEnoughPoints {
                needed: 3,
                given: n,
            });
        }
        let segments = (0..n)
            .map(|i| {
                CubicSegment::from_matrix(
                    [
                        self.points[i],
                        self.points[(i + 1) % n],
                        self.points[(i + 2) % n],
                        self.points[(i + 3) % n],
                    ],
                    B_SPLINE,
                )
            })
            .collect();
        Ok(CubicCurve { segments })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 四个点排成一条上凸的弧。
    fn points() -> Vec<Vec2> {
        vec![
            Vec2::new(-3.0, 0.0),
            Vec2::new(-1.0, 2.0),
            Vec2::new(1.0, 2.0),
            Vec2::new(3.0, 0.0),
        ]
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        a.distance(b) < 1e-4
    }

    // ── Hermite ──

    #[test]
    fn hermite_passes_through_its_points_with_the_given_tangents() {
        // Hermite 的全部意义就在这四条断言上：两端的位置和速度都是你给的。
        // 矩阵抄错一个数，这里立刻就炸。
        let p = [Vec2::new(0.0, 0.0), Vec2::new(4.0, 0.0)];
        let v = [Vec2::new(0.0, 10.0), Vec2::new(0.0, -10.0)];
        let curve = CubicHermite::new(p, v).to_curve().unwrap();

        assert!(close(curve.position(0.0), p[0]));
        assert!(close(curve.position(1.0), p[1]));
        assert!(close(curve.velocity(0.0), v[0]), "起点切线不对");
        assert!(close(curve.velocity(1.0), v[1]), "终点切线不对");
    }

    #[test]
    fn hermite_needs_at_least_two_points() {
        let one = CubicHermite::new(vec![Vec2::ZERO], vec![Vec2::X]).to_curve();
        assert_eq!(
            one,
            Err(NotEnoughPoints {
                needed: 2,
                given: 1
            })
        );
    }

    #[test]
    fn a_cyclic_hermite_closes_the_loop() {
        let p = points();
        let tangents = vec![Vec2::X; 4];
        let curve = CubicHermite::new(p.clone(), tangents)
            .to_curve_cyclic()
            .unwrap();

        assert_eq!(curve.segments().len(), 4, "闭合要多出回到起点的那一段");
        assert!(
            close(curve.position(curve.domain_end()), p[0]),
            "绕一圈没回到起点"
        );
    }

    // ── Catmull-Rom / Cardinal ──

    #[test]
    fn catmull_rom_passes_through_every_control_point() {
        // 「只给点就能穿过它们」正是这类曲线被选中的理由。
        let p = points();
        let curve = CubicCardinalSpline::catmull_rom(p.clone())
            .to_curve()
            .unwrap();

        for (i, expected) in p.iter().enumerate() {
            let at = curve.position(i as f32);
            assert!(close(at, *expected), "第 {i} 个控制点没被穿过：{at}");
        }
    }

    #[test]
    fn zero_tension_gives_straight_segments() {
        // 松紧为 0 时切线全是零，每段退化成直线——控制点之间是折线。
        let p = points();
        let curve = CubicCardinalSpline::new(0.0, p.clone()).to_curve().unwrap();

        let midpoint = curve.position(0.5);
        let straight = (p[0] + p[1]) * 0.5;
        assert!(close(midpoint, straight), "松紧为 0 该是直线");
    }

    #[test]
    fn a_cyclic_cardinal_returns_to_the_start() {
        let p = points();
        let curve = CubicCardinalSpline::catmull_rom(p.clone())
            .to_curve_cyclic()
            .unwrap();

        assert_eq!(curve.segments().len(), 4);
        assert!(close(curve.position(4.0), p[0]));
        // 闭合曲线上每个控制点仍然被穿过。
        assert!(close(curve.position(2.0), p[2]));
    }

    // ── B 样条 ──

    #[test]
    fn a_b_spline_does_not_pass_through_its_control_points() {
        // 起点是前三个点的加权平均 (p₀ + 4p₁ + p₂)/6，不是 p₀。
        // 这条断言看着别扭，但它正是 B 样条与前两种的分界线。
        let p = points();
        let curve = CubicBSpline::new(p.clone()).to_curve().unwrap();

        let expected = (p[0] + p[1] * 4.0 + p[2]) * (1.0 / 6.0);
        assert!(close(curve.position(0.0), expected));
        assert!(!close(curve.position(0.0), p[0]), "B 样条不该过控制点");
    }

    #[test]
    fn a_b_spline_needs_four_points() {
        let three = CubicBSpline::new(vec![Vec2::ZERO; 3]).to_curve();
        assert_eq!(
            three,
            Err(NotEnoughPoints {
                needed: 4,
                given: 3
            })
        );
    }

    #[test]
    fn a_b_spline_is_smooth_across_the_seam() {
        // B 样条的卖点是二阶连续：接缝处不只速度不跳，加速度也不跳。
        let p = vec![
            Vec2::new(-3.0, 0.0),
            Vec2::new(-1.0, 2.0),
            Vec2::new(1.0, 2.0),
            Vec2::new(3.0, 0.0),
            Vec2::new(5.0, 3.0),
        ];
        let curve = CubicBSpline::new(p).to_curve().unwrap();

        let before = curve.acceleration(0.999);
        let after = curve.acceleration(1.001);
        assert!(
            before.distance(after) < 0.05,
            "接缝处加速度跳了：{before} → {after}"
        );
    }

    // ── Bezier ──

    #[test]
    fn a_bezier_hits_its_endpoints_but_not_its_handles() {
        let segment = [
            Vec2::new(0.0, 0.0),
            Vec2::new(0.0, 4.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(4.0, 0.0),
        ];
        let curve = CubicBezier::new(vec![segment]).to_curve().unwrap();

        assert!(close(curve.position(0.0), segment[0]));
        assert!(close(curve.position(1.0), segment[3]));
        // 中点被两个手柄拉高，但够不到它们那么高。
        let middle = curve.position(0.5);
        assert!(middle.y > 2.0 && middle.y < 4.0, "中点在 {middle}");
    }

    #[test]
    fn a_one_dimensional_curve_works_as_an_easing_function() {
        // f32 也是 `Point`，一维三次曲线就是一条缓动。
        let ease = CubicBezier::new(vec![[0.0_f32, 0.0, 1.0, 1.0]])
            .to_curve()
            .unwrap();

        assert!(ease.position(0.0).abs() < 1e-6);
        assert!((ease.position(1.0) - 1.0).abs() < 1e-6);
        assert!(
            (ease.position(0.5) - 0.5).abs() < 1e-6,
            "对称的缓动中点该是 0.5"
        );
    }

    // ── 取值与采样 ──

    #[test]
    fn the_parameter_is_clamped_instead_of_extrapolated() {
        // 三次多项式在定义域外跑得飞快：不夹住的话，t 稍微越界一点，
        // 一条本来老实的路径就飞出屏幕了。
        let curve = CubicCardinalSpline::catmull_rom(points())
            .to_curve()
            .unwrap();

        assert!(close(curve.position(-5.0), curve.position(0.0)));
        assert!(close(
            curve.position(99.0),
            curve.position(curve.domain_end())
        ));
    }

    #[test]
    fn sampling_covers_both_ends() {
        let curve = CubicCardinalSpline::catmull_rom(points())
            .to_curve()
            .unwrap();
        let sampled: Vec<Vec2> = curve.iter_positions(50).collect();

        assert_eq!(sampled.len(), 50);
        assert!(close(sampled[0], curve.position(0.0)), "没从头开始");
        assert!(
            close(sampled[49], curve.position(curve.domain_end())),
            "没采到末端"
        );
    }

    #[test]
    fn an_empty_curve_answers_zero_instead_of_panicking() {
        // 交互式编辑里控制点可能被删光。返回零值总比 panic 强——
        // 那一帧画不出线，下一帧加回一个点就恢复了。
        let curve: CubicCurve<Vec2> = CubicCurve {
            segments: Vec::new(),
        };
        assert_eq!(curve.position(0.5), Vec2::ZERO);
        assert_eq!(curve.velocity(0.5), Vec2::ZERO);
        assert!(curve.is_empty());
    }

    #[test]
    fn velocity_matches_a_numerical_derivative() {
        // 解析导数和数值差分对得上，说明求导那几行没抄错。
        let curve = CubicCardinalSpline::catmull_rom(points())
            .to_curve()
            .unwrap();

        let t = 1.3;
        let h = 1e-3;
        let numerical = (curve.position(t + h) - curve.position(t - h)) * (1.0 / (2.0 * h));
        let analytic = curve.velocity(t);

        assert!(
            numerical.distance(analytic) < 1e-2,
            "解析 {analytic} vs 数值 {numerical}"
        );
    }
}
