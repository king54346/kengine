//! 浮层定位：把一块内容摆在某个控件的旁边，并保证它不出屏。
//!
//! 下拉框、菜单、工具提示都靠它。照 `bevy_ui_widgets::popover` 的模型：
//! 给一组**候选位置**，挑第一个放得下的；一个都放不下就挑「最不糟」的。
//!
//! ```
//! # use kui_widgets::popover::{Placement, Side, Align, place};
//! # use kui::Rect;
//! # use kmath::Vec2;
//! let anchor = Rect::new(100.0, 100.0, 100.0, 30.0);
//! let rect = place(
//!     anchor,
//!     Vec2::new(160.0, 200.0),
//!     &[
//!         Placement::new(Side::Bottom, Align::Start),
//!         Placement::new(Side::Top, Align::Start),
//!     ],
//!     Vec2::new(1280.0, 720.0),
//!     4.0,
//! );
//! assert!(rect.min.y >= anchor.max.y);
//! ```
//!
//! # 为什么要「一组」候选位置而不是一个
//!
//! 一个贴着屏幕底边的按钮，它的下拉菜单必须往**上**弹。只给一个位置的话，
//! 要么菜单被裁掉一半，要么每个调用方都得自己判断还剩多少空间。
//!
//! # 纯几何，不碰绘制
//!
//! 这里只算矩形。谁来画、画什么，由调用方决定——同一套定位逻辑
//! 下拉框、菜单、提示气泡都能用。这也让它**完全可测**：不需要 GPU，
//! 不需要字体。

use kmath::Vec2;
use kui::Rect;

/// 浮层放在锚点的哪一侧。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// 上方。
    Top,
    /// 下方。下拉框的默认方向。
    #[default]
    Bottom,
    /// 左侧。
    Left,
    /// 右侧。子菜单的默认方向。
    Right,
}

impl Side {
    /// 相反的一侧。放不下时先试它——上下翻转比左右乱跳自然得多。
    pub fn mirror(self) -> Self {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    /// 这一侧是不是竖直方向（上或下）。
    pub fn is_vertical(self) -> bool {
        matches!(self, Side::Top | Side::Bottom)
    }
}

/// 浮层在**垂直于** [`Side`] 的那根轴上怎么对齐。
///
/// 比如浮层在下方时，这个控制的是水平对齐。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    /// 起始边对齐（左对齐 / 上对齐）。
    #[default]
    Start,
    /// 居中。
    Center,
    /// 结束边对齐（右对齐 / 下对齐）。
    End,
}

/// 一个候选位置。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Placement {
    /// 放在哪一侧。
    pub side: Side,
    /// 另一根轴上怎么对齐。
    pub align: Align,
    /// 和锚点之间留多少间隙。
    pub gap: f32,
}

impl Placement {
    /// 建一个候选位置，间隙为 0。
    pub fn new(side: Side, align: Align) -> Self {
        Self {
            side,
            align,
            gap: 0.0,
        }
    }

    /// 设置间隙。
    pub fn with_gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// 按这个位置算出浮层的矩形，**不考虑**是否出屏。
    pub fn resolve(&self, anchor: Rect, size: Vec2) -> Rect {
        let anchor_size = anchor.size();

        let (x, y) = match self.side {
            Side::Top => (
                align_along(anchor.min.x, anchor_size.x, size.x, self.align),
                anchor.min.y - size.y - self.gap,
            ),
            Side::Bottom => (
                align_along(anchor.min.x, anchor_size.x, size.x, self.align),
                anchor.max.y + self.gap,
            ),
            Side::Left => (
                anchor.min.x - size.x - self.gap,
                align_along(anchor.min.y, anchor_size.y, size.y, self.align),
            ),
            Side::Right => (
                anchor.max.x + self.gap,
                align_along(anchor.min.y, anchor_size.y, size.y, self.align),
            ),
        };

        Rect {
            min: Vec2::new(x, y),
            max: Vec2::new(x + size.x, y + size.y),
        }
    }
}

/// 在垂直轴上按 `align` 摆一段长度。
fn align_along(anchor_start: f32, anchor_length: f32, length: f32, align: Align) -> f32 {
    match align {
        Align::Start => anchor_start,
        Align::Center => anchor_start + (anchor_length - length) * 0.5,
        Align::End => anchor_start + anchor_length - length,
    }
}

/// 挑一个位置摆下浮层。
///
/// 依次试 `candidates`，返回**第一个完全放得下**的。都放不下时取超出面积
/// 最小的那个，再夹进屏幕——一个被裁掉一半的菜单比一个位置不理想的菜单
/// 糟得多。
///
/// `margin` 是允许贴近屏幕边缘的最小距离。
///
/// `candidates` 为空时退化成「放在下方、左对齐」。
pub fn place(
    anchor: Rect,
    size: Vec2,
    candidates: &[Placement],
    screen: Vec2,
    margin: f32,
) -> Rect {
    let fallback = [Placement::default()];
    let candidates = if candidates.is_empty() {
        &fallback[..]
    } else {
        candidates
    };

    let bounds = Rect {
        min: Vec2::splat(margin),
        max: (screen - Vec2::splat(margin)).max(Vec2::splat(margin)),
    };

    let mut best: Option<(f32, Rect)> = None;
    for placement in candidates {
        let rect = placement.resolve(anchor, size);
        let overflow = overflow_area(rect, bounds);
        if overflow <= 0.0 {
            return rect;
        }
        // 记下超出面积最小的那个。用面积而不是「超出多少像素」：
        // 一个只在角上超出一点的位置，比一整条边都超出的位置好。
        if best.is_none_or(|(worst, _)| overflow < worst) {
            best = Some((overflow, rect));
        }
    }

    // 一个都放不下：把最不糟的那个夹进屏幕。
    let (_, rect) = best.expect("candidates 非空，循环至少跑一轮");
    clamp_into(rect, bounds)
}

/// 一个矩形超出边界的面积。完全在里面时是 0。
fn overflow_area(rect: Rect, bounds: Rect) -> f32 {
    let left = (bounds.min.x - rect.min.x).max(0.0);
    let top = (bounds.min.y - rect.min.y).max(0.0);
    let right = (rect.max.x - bounds.max.x).max(0.0);
    let bottom = (rect.max.y - bounds.max.y).max(0.0);

    let size = rect.size();
    // 横竖两条超出带的面积，减掉重复算的角。
    let horizontal = (left + right) * size.y;
    let vertical = (top + bottom) * size.x;
    let corners = (left + right) * (top + bottom);
    horizontal + vertical - corners
}

/// 把矩形整体挪进边界。
///
/// 挪而不是裁：裁的话浮层会缺一块，内容被切断；挪的话内容完整，
/// 只是位置不理想。浮层比屏幕还大时，保证**左上角**可见——
/// 内容通常从那里开始。
fn clamp_into(rect: Rect, bounds: Rect) -> Rect {
    let size = rect.size();
    let max_x = (bounds.max.x - size.x).max(bounds.min.x);
    let max_y = (bounds.max.y - size.y).max(bounds.min.y);

    let min = Vec2::new(
        rect.min.x.clamp(bounds.min.x, max_x),
        rect.min.y.clamp(bounds.min.y, max_y),
    );
    Rect {
        min,
        max: min + size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> Rect {
        Rect::new(100.0, 100.0, 100.0, 30.0)
    }

    const SCREEN: Vec2 = Vec2::new(1280.0, 720.0);

    #[test]
    fn bottom_start_sits_under_the_anchor() {
        let rect =
            Placement::new(Side::Bottom, Align::Start).resolve(anchor(), Vec2::new(80.0, 40.0));
        assert_eq!(rect.min.x, 100.0, "左对齐该贴着锚点左边");
        assert_eq!(rect.min.y, 130.0, "该贴在锚点下边");
    }

    #[test]
    fn top_places_above() {
        let rect = Placement::new(Side::Top, Align::Start).resolve(anchor(), Vec2::new(80.0, 40.0));
        assert_eq!(rect.max.y, 100.0, "该贴在锚点上边");
    }

    #[test]
    fn the_gap_pushes_it_away() {
        let rect = Placement::new(Side::Bottom, Align::Start)
            .with_gap(6.0)
            .resolve(anchor(), Vec2::new(80.0, 40.0));
        assert_eq!(rect.min.y, 136.0);
    }

    #[test]
    fn align_works_on_the_perpendicular_axis() {
        // 浮层在下方时，align 控制的是**水平**对齐。
        let size = Vec2::new(80.0, 40.0);
        let start = Placement::new(Side::Bottom, Align::Start).resolve(anchor(), size);
        let center = Placement::new(Side::Bottom, Align::Center).resolve(anchor(), size);
        let end = Placement::new(Side::Bottom, Align::End).resolve(anchor(), size);

        assert_eq!(start.min.x, 100.0);
        assert_eq!(center.min.x, 110.0, "锚点宽 100、浮层宽 80，居中偏移 10");
        assert_eq!(end.max.x, 200.0);

        // 三者的竖直位置一样——align 不该影响那根轴。
        assert_eq!(start.min.y, center.min.y);
        assert_eq!(center.min.y, end.min.y);
    }

    #[test]
    fn align_works_vertically_for_side_placements() {
        let size = Vec2::new(80.0, 10.0);
        let start = Placement::new(Side::Right, Align::Start).resolve(anchor(), size);
        let end = Placement::new(Side::Right, Align::End).resolve(anchor(), size);

        assert_eq!(start.min.y, 100.0);
        assert_eq!(end.max.y, 130.0);
        assert_eq!(start.min.x, end.min.x, "align 不该影响水平位置");
    }

    #[test]
    fn mirror_flips_the_side() {
        assert_eq!(Side::Top.mirror(), Side::Bottom);
        assert_eq!(Side::Bottom.mirror(), Side::Top);
        assert_eq!(Side::Left.mirror(), Side::Right);
        assert_eq!(Side::Right.mirror(), Side::Left);
    }

    // ── 挑位置 ──

    #[test]
    fn the_first_fitting_candidate_wins() {
        let rect = place(
            anchor(),
            Vec2::new(80.0, 40.0),
            &[
                Placement::new(Side::Bottom, Align::Start),
                Placement::new(Side::Top, Align::Start),
            ],
            SCREEN,
            4.0,
        );
        assert_eq!(rect.min.y, 130.0, "第一个就放得下，不该换到上面");
    }

    #[test]
    fn a_menu_near_the_bottom_flips_upward() {
        // 贴着屏幕底边的按钮，它的下拉菜单必须往上弹。
        let anchor = Rect::new(100.0, 690.0, 100.0, 25.0);
        let rect = place(
            anchor,
            Vec2::new(80.0, 200.0),
            &[
                Placement::new(Side::Bottom, Align::Start),
                Placement::new(Side::Top, Align::Start),
            ],
            SCREEN,
            4.0,
        );
        assert!(rect.max.y <= anchor.min.y, "没往上翻：{rect:?}");
    }

    #[test]
    fn a_menu_near_the_right_edge_flips_left() {
        let anchor = Rect::new(1200.0, 100.0, 70.0, 30.0);
        let rect = place(
            anchor,
            Vec2::new(200.0, 100.0),
            &[
                Placement::new(Side::Right, Align::Start),
                Placement::new(Side::Left, Align::Start),
            ],
            SCREEN,
            4.0,
        );
        assert!(rect.max.x <= anchor.min.x, "没往左翻：{rect:?}");
    }

    #[test]
    fn when_nothing_fits_it_is_clamped_on_screen() {
        // 一个被裁掉一半的菜单比一个位置不理想的菜单糟得多。
        let anchor = Rect::new(10.0, 10.0, 20.0, 20.0);
        let rect = place(
            anchor,
            Vec2::new(400.0, 700.0),
            &[Placement::new(Side::Top, Align::Start)],
            SCREEN,
            4.0,
        );
        assert!(rect.min.x >= 4.0 - 1e-3, "左边出屏了：{rect:?}");
        assert!(rect.min.y >= 4.0 - 1e-3, "上边出屏了：{rect:?}");
        assert!(rect.max.y <= 716.0 + 1e-3, "下边出屏了：{rect:?}");
    }

    #[test]
    fn a_popover_bigger_than_the_screen_keeps_its_top_left_visible() {
        // 内容通常从左上角开始。
        let rect = place(
            anchor(),
            Vec2::new(2000.0, 2000.0),
            &[Placement::default()],
            SCREEN,
            4.0,
        );
        assert_eq!(rect.min, Vec2::splat(4.0), "左上角该贴着边距：{rect:?}");
    }

    #[test]
    fn the_size_is_never_changed() {
        // 挪而不是裁：裁的话浮层会缺一块，内容被切断。
        let size = Vec2::new(400.0, 700.0);
        let rect = place(
            Rect::new(10.0, 10.0, 20.0, 20.0),
            size,
            &[Placement::new(Side::Top, Align::Start)],
            SCREEN,
            4.0,
        );
        assert!(
            (rect.size() - size).length() < 1e-3,
            "尺寸被改了：{:?}",
            rect.size()
        );
    }

    #[test]
    fn the_least_bad_candidate_is_picked() {
        // 都放不下时该挑超出最少的那个。
        let anchor = Rect::new(600.0, 690.0, 100.0, 25.0);
        let size = Vec2::new(100.0, 300.0);
        let rect = place(
            anchor,
            size,
            &[
                // 往下：超出约 275 像素高。
                Placement::new(Side::Bottom, Align::Start),
                // 往上：完全放得下。
                Placement::new(Side::Top, Align::Start),
            ],
            SCREEN,
            4.0,
        );
        assert!(rect.max.y <= anchor.min.y);
    }

    #[test]
    fn no_candidates_falls_back_to_below() {
        let rect = place(anchor(), Vec2::new(80.0, 40.0), &[], SCREEN, 4.0);
        assert_eq!(rect.min.y, 130.0);
        assert_eq!(rect.min.x, 100.0);
    }

    #[test]
    fn overflow_area_is_zero_when_inside() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            overflow_area(Rect::new(10.0, 10.0, 10.0, 10.0), bounds),
            0.0
        );
    }

    #[test]
    fn overflow_area_grows_with_the_part_outside() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let little = overflow_area(Rect::new(95.0, 10.0, 10.0, 10.0), bounds);
        let lots = overflow_area(Rect::new(50.0, 10.0, 100.0, 10.0), bounds);
        assert!(lots > little, "{lots} 该比 {little} 大");
    }

    #[test]
    fn a_degenerate_screen_does_not_produce_nonsense() {
        // 窗口最小化时尺寸可能是 0。不该算出翻转的矩形。
        for screen in [Vec2::ZERO, Vec2::new(1.0, 1.0), Vec2::new(-10.0, -10.0)] {
            let rect = place(
                anchor(),
                Vec2::new(80.0, 40.0),
                &[Placement::default()],
                screen,
                4.0,
            );
            assert!(rect.max.x >= rect.min.x, "矩形翻转了：{rect:?}");
            assert!(rect.max.y >= rect.min.y, "矩形翻转了：{rect:?}");
            assert!(rect.min.is_finite() && rect.max.is_finite());
        }
    }

    #[test]
    fn a_zero_sized_popover_is_handled() {
        let rect = place(anchor(), Vec2::ZERO, &[Placement::default()], SCREEN, 4.0);
        assert_eq!(rect.size(), Vec2::ZERO);
    }
}
