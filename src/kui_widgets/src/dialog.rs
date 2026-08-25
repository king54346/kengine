//! 对话框的位置：拖动、夹进屏幕、居中。
//!
//! 和 [`menu`](crate::menu) 一样只做纯计算。画由
//! [`WidgetUi::modal`](crate::WidgetUi::modal) 和
//! [`dialog_title`](crate::WidgetUi::dialog_title) 出，
//! 位置由调用方保管——这里提供的是「保管时该怎么算」。

use kmath::Vec2;
use kui::Rect;

/// 拖动时至少要留在屏幕里的长度。
///
/// 全夹进屏幕的话，对话框就永远贴着边，没法把它拖出去一点来看下面的
/// 东西；完全不夹的话，标题栏被拖出屏幕后就再也抓不回来了。留一段
/// 标题栏在屏幕里是两者的折中。
pub const MIN_VISIBLE: f32 = 48.0;

/// 把对话框摆在屏幕正中。
///
/// 对话框比屏幕还大时会返回负的坐标，这是故意的——夹到 0 会让下边和
/// 右边超出去更多，居中至少让两头超出的一样多。
pub fn center(size: Vec2, screen: Vec2) -> Vec2 {
    (screen - size) * 0.5
}

/// 拖动之后的新位置。
///
/// `position` 是拖动前的左上角，`drag` 是这一帧的位移增量，
/// `size` 是对话框大小，`screen` 是窗口大小。
///
/// 结果保证标题栏还有 [`MIN_VISIBLE`] 那么长留在屏幕里，而且
/// **上边不会跑到屏幕外**——标题栏在顶上，它出去了就再也抓不住了。
pub fn drag(position: Vec2, drag: Vec2, size: Vec2, screen: Vec2) -> Vec2 {
    clamp(position + drag, size, screen)
}

/// 把位置夹到「还抓得住」的范围里。
///
/// 窗口被缩小之后也要调一次，否则原本在中间的对话框会整个落到屏幕外。
pub fn clamp(position: Vec2, size: Vec2, screen: Vec2) -> Vec2 {
    // 左右两边各允许拖出去，只要还剩一截在屏幕里。
    //
    // 要留的那一截还得比屏幕本身短：窗口只剩 20 像素宽时，「留 48 像素
    // 在屏幕里」是做不到的，硬按 48 算会把范围算反、把对话框推到屏幕外。
    let keep = MIN_VISIBLE.min(size.x).min(screen.x.max(0.0));
    let min_x = keep - size.x;
    let max_x = screen.x - keep;

    // 上边一律不许出屏幕：标题栏在顶上，它出去了就再也抓不住了。
    // 下边可以，因为抓的地方还在。
    let min_y = 0.0;
    let max_y = (screen.y - MIN_VISIBLE.min(size.y)).max(0.0);

    Vec2::new(
        position.x.clamp(min_x.min(max_x), max_x),
        position.y.clamp(min_y, max_y.max(min_y)),
    )
}

/// 对话框占的矩形。
pub fn rect(position: Vec2, size: Vec2) -> Rect {
    Rect {
        min: position,
        max: position + size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Vec2 = Vec2::new(1280.0, 720.0);
    const SIZE: Vec2 = Vec2::new(400.0, 300.0);

    #[test]
    fn centering_puts_equal_space_on_both_sides() {
        let at = center(SIZE, SCREEN);
        assert_eq!(at, Vec2::new(440.0, 210.0));
        // 左右留白相等。
        assert_eq!(at.x, SCREEN.x - (at.x + SIZE.x));
    }

    /// 对话框比屏幕大时居中会得到负坐标，两头超出一样多。
    #[test]
    fn centering_an_oversized_dialog_overflows_evenly() {
        let big = Vec2::new(1600.0, 900.0);
        let at = center(big, SCREEN);
        assert!(at.x < 0.0 && at.y < 0.0);
        assert_eq!(at.x, SCREEN.x - (at.x + big.x));
    }

    /// 屏幕中间拖一下，就是简单地加上增量。
    #[test]
    fn dragging_in_open_space_just_adds_the_delta() {
        let at = drag(
            Vec2::new(400.0, 300.0),
            Vec2::new(30.0, -20.0),
            SIZE,
            SCREEN,
        );
        assert_eq!(at, Vec2::new(430.0, 280.0));
    }

    /// 往左拖出屏幕，右边要留一截抓得住。
    #[test]
    fn dragging_off_the_left_keeps_a_grip() {
        let at = drag(Vec2::new(0.0, 100.0), Vec2::new(-9999.0, 0.0), SIZE, SCREEN);
        // 右边缘还在屏幕里。
        assert_eq!(at.x + SIZE.x, MIN_VISIBLE);
        assert!(at.x < 0.0);
    }

    /// 往右拖出屏幕同理，左边留一截。
    #[test]
    fn dragging_off_the_right_keeps_a_grip() {
        let at = drag(Vec2::new(0.0, 100.0), Vec2::new(9999.0, 0.0), SIZE, SCREEN);
        assert_eq!(at.x, SCREEN.x - MIN_VISIBLE);
    }

    /// 上边一律不许出屏幕——标题栏在顶上，出去了就抓不回来。
    #[test]
    fn a_dialog_can_never_be_dragged_above_the_screen() {
        let at = drag(
            Vec2::new(400.0, 10.0),
            Vec2::new(0.0, -9999.0),
            SIZE,
            SCREEN,
        );
        assert_eq!(at.y, 0.0);
    }

    /// 下边可以拖出去，标题栏还留在屏幕里。
    #[test]
    fn a_dialog_can_be_dragged_below_the_screen() {
        let at = drag(
            Vec2::new(400.0, 400.0),
            Vec2::new(0.0, 9999.0),
            SIZE,
            SCREEN,
        );
        assert_eq!(at.y, SCREEN.y - MIN_VISIBLE);
        // 确实有一部分在屏幕外面。
        assert!(at.y + SIZE.y > SCREEN.y);
    }

    /// 比 MIN_VISIBLE 还小的对话框不能被要求「留 48 像素在屏幕里」——
    /// 它总共才 20 像素。夹的时候要按它自己的尺寸来。
    #[test]
    fn a_tiny_dialog_is_clamped_by_its_own_size() {
        let tiny = Vec2::new(20.0, 20.0);
        let at = drag(Vec2::new(0.0, 100.0), Vec2::new(-9999.0, 0.0), tiny, SCREEN);
        // 整个贴在左边缘，而不是被推到屏幕外 28 像素。
        assert_eq!(at.x, 0.0);
    }

    /// 对话框比屏幕还大时，「留一截抓得住」这条规则照样生效——
    /// 不因为它大就改成强制贴左上角。用户是**故意**把它拖过去的，
    /// 拽回来等于跟用户较劲。
    #[test]
    fn an_oversized_dialog_still_obeys_the_grip_rule() {
        let screen = Vec2::new(200.0, 150.0);
        let at = clamp(Vec2::new(-500.0, -500.0), SIZE, screen);
        assert_eq!(at.x + SIZE.x, MIN_VISIBLE);
        // 上边仍然不许出屏幕。
        assert_eq!(at.y, 0.0);
    }

    /// 不管尺寸怎么组合，夹完之后总有一截留在屏幕里。
    ///
    /// 这条盯着的是范围反转：`keep` 如果不跟着屏幕一起缩，窄窗口下
    /// 会算出 `min > max`，`clamp` 要么 panic 要么给出屏幕外的结果。
    #[test]
    fn something_always_stays_on_screen() {
        let sizes = [1.0, 20.0, 48.0, 400.0, 2000.0];
        let screens = [0.0, 1.0, 20.0, 47.0, 200.0, 1280.0];
        for size in sizes {
            for screen in screens {
                let size = Vec2::splat(size);
                let screen = Vec2::splat(screen);
                // 往四个方向都使劲拖一遍。
                for push in [-9999.0, 9999.0] {
                    let at = clamp(Vec2::splat(push), size, screen);
                    assert!(at.x.is_finite() && at.y.is_finite(), "{size:?} {screen:?}");

                    // 屏幕和对话框重叠的部分不为空。
                    let visible_x = (at.x + size.x).min(screen.x) - at.x.max(0.0);
                    let expected = MIN_VISIBLE.min(size.x).min(screen.x);
                    assert!(
                        visible_x >= expected - 0.001,
                        "只剩 {visible_x} 可见，应至少 {expected}（{size:?} 在 {screen:?} 里）",
                    );
                    // 上边永远不出屏幕。
                    assert!(at.y >= 0.0, "{at:?}");
                }
            }
        }
    }

    /// 窗口被缩到 0（最小化）时不该 panic，也不该给出 NaN，
    /// 更不该把对话框推到负坐标去——恢复窗口后就找不到它了。
    #[test]
    fn a_zero_sized_screen_is_not_a_panic() {
        let at = clamp(Vec2::new(100.0, 100.0), SIZE, Vec2::ZERO);
        assert!(at.x.is_finite() && at.y.is_finite());
        assert_eq!(at, Vec2::ZERO);
    }

    /// 窗口缩小之后重新夹一次，把跑到外面的对话框拉回来。
    #[test]
    fn shrinking_the_window_pulls_the_dialog_back() {
        let at = Vec2::new(1000.0, 600.0);
        let small = Vec2::new(640.0, 480.0);
        let pulled = clamp(at, SIZE, small);
        assert!(pulled.x <= small.x - MIN_VISIBLE);
        assert!(pulled.y <= small.y - MIN_VISIBLE);
    }

    /// 反复拖动不该累积漂移：夹住之后再拖 0 应当原地不动。
    #[test]
    fn a_clamped_dialog_does_not_drift() {
        let mut at = drag(
            Vec2::new(400.0, 300.0),
            Vec2::new(9999.0, 9999.0),
            SIZE,
            SCREEN,
        );
        for _ in 0..10 {
            let next = drag(at, Vec2::ZERO, SIZE, SCREEN);
            assert_eq!(next, at);
            at = next;
        }
    }

    #[test]
    fn the_rect_covers_the_whole_dialog() {
        let r = rect(Vec2::new(100.0, 50.0), SIZE);
        assert_eq!(r.min, Vec2::new(100.0, 50.0));
        assert_eq!(r.max, Vec2::new(500.0, 350.0));
        assert_eq!(r.size(), SIZE);
    }
}
