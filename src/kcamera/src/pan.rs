//! 平移相机：在一个平面上拖着看，带缩放。
//!
//! 2D 游戏、关卡编辑器、策略地图用它。
//!
//! # 和轨道相机的分工
//!
//! [`OrbitCamera`](crate::OrbitCamera) 也能平移（它的 `pan` 移动目标点），
//! 但它的 `zoom` 改的是**离目标的距离**——正交投影下把相机拉远拉近，
//! 画面里的东西一点都不会变大变小。所以 2D 的缩放必须走另一条路：
//! 改**可视范围**。这就是这个类型单独存在的理由。
//!
//! ```
//! # use kcamera::{Camera, PanCamera};
//! let mut pan = PanCamera::new(10.0);
//!
//! pan.drag(50.0, 0.0);   // 拖动（像素）
//! pan.zoom_by(-1.0);     // 滚轮，负数是拉近
//!
//! // 缩放体现在可视高度上，不在相机位置上。
//! let camera = Camera::orthographic(pan.viewport_height());
//! ```

use kmath::{Mat4, Quat, Vec2, Vec3};

/// 一台在平面上平移的相机。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanCamera {
    /// 视野中心（平面坐标）。
    pub focus: Vec2,
    /// 缩放系数：1 是原始大小，越小看得越近。
    pub zoom: f32,
    /// 缩放下限（最多能放多大）。
    pub min_zoom: f32,
    /// 缩放上限（最多能拉多远）。
    pub max_zoom: f32,
    /// `zoom == 1` 时的可视高度（世界单位）。
    pub base_height: f32,
    /// 拖动的换算比例（世界单位／像素／可视高度）。
    pub drag_speed: f32,
    /// 键盘平移速度（可视高度／秒）。
    pub move_speed: f32,
}

impl Default for PanCamera {
    fn default() -> Self {
        Self {
            focus: Vec2::ZERO,
            zoom: 1.0,
            min_zoom: 0.1,
            max_zoom: 10.0,
            base_height: 10.0,
            drag_speed: 0.002,
            move_speed: 0.8,
        }
    }
}

impl PanCamera {
    /// 指定 `zoom == 1` 时的可视高度。
    pub fn new(base_height: f32) -> Self {
        Self {
            base_height,
            ..Self::default()
        }
    }

    /// 设定缩放范围。
    pub fn with_zoom_range(mut self, min: f32, max: f32) -> Self {
        self.min_zoom = min.min(max).max(1e-3);
        self.max_zoom = min.max(max);
        self.clamp();
        self
    }

    /// 拖动。接鼠标位移的像素增量。
    ///
    /// 方向和内容相反：手往右拖，画面里的东西跟着往右走，也就是视野
    /// 往左移。这是所有地图与画布的通行做法，反过来会让人立刻别扭。
    ///
    /// 位移按**当前可视高度**缩放：放大之后同样的拖动该走得更少，
    /// 否则细看时手一抖就飞出视野。
    pub fn drag(&mut self, dx: f32, dy: f32) {
        let scale = self.viewport_height() * self.drag_speed;
        self.focus.x -= dx * scale;
        // 屏幕 y 向下、世界 y 向上，所以这里是加。
        self.focus.y += dy * scale;
    }

    /// 键盘平移。`axes` 取 `-1..=1`，`dt` 是帧间隔。
    pub fn travel(&mut self, axes: Vec2, dt: f32) {
        if !dt.is_finite() {
            return;
        }
        let step = self.viewport_height() * self.move_speed * dt;
        self.focus += axes * step;
    }

    /// 缩放。`delta` 为负是拉近。
    ///
    /// 按比例缩放而不是加减固定值：拉近到最后几步用加法会变得极其迟钝，
    /// 拉远时又快得刹不住。
    pub fn zoom_by(&mut self, delta: f32) {
        if !delta.is_finite() {
            return;
        }
        self.zoom *= (1.0 + delta * 0.1).clamp(0.1, 10.0);
        self.clamp();
    }

    /// 当前的可视高度，交给正交相机。
    pub fn viewport_height(&self) -> f32 {
        self.base_height * self.zoom
    }

    /// 相机在世界空间的位置。
    ///
    /// `distance` 是相机离平面多远。正交投影下它不影响画面大小，只要
    /// 别小于近裁剪面、别让要看的东西跑到相机背后就行。
    pub fn position(&self, distance: f32) -> Vec3 {
        self.focus.extend(distance)
    }

    /// 相机的世界变换，直接设给节点。朝向恒定正对 XY 平面。
    pub fn transform(&self, distance: f32) -> Mat4 {
        Mat4::from_rotation_translation(Quat::IDENTITY, self.position(distance))
    }

    fn clamp(&mut self) {
        self.zoom = self.zoom.clamp(self.min_zoom, self.max_zoom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_changes_the_viewport_not_the_position() {
        // 正交投影下把相机拉远拉近，画面里的东西一点都不会变——
        // 2D 的缩放必须改可视范围。这条是这个类型存在的理由。
        let mut pan = PanCamera::new(10.0);
        let before = pan.position(50.0);

        pan.zoom_by(5.0);

        assert_eq!(pan.position(50.0), before, "缩放不该挪动相机");
        assert!(pan.viewport_height() > 10.0, "可视范围没变大");
    }

    #[test]
    fn dragging_moves_opposite_to_the_pointer() {
        // 手往右拖，画面内容跟着往右走，也就是视野往左移。
        let mut pan = PanCamera::new(10.0);
        pan.drag(100.0, 0.0);

        assert!(pan.focus.x < 0.0, "拖动方向反了");
    }

    #[test]
    fn dragging_follows_the_screen_y_convention() {
        // 屏幕 y 向下、世界 y 向上：手往下拖，视野该往上移。
        let mut pan = PanCamera::new(10.0);
        pan.drag(0.0, 100.0);

        assert!(pan.focus.y > 0.0, "纵向拖动的方向反了");
    }

    #[test]
    fn drag_distance_follows_the_zoom() {
        // 放大之后同样的拖动该走得更少，否则细看时手一抖就飞出视野。
        let travel = |zoom: f32| {
            let mut pan = PanCamera {
                zoom,
                ..PanCamera::new(10.0)
            };
            pan.drag(100.0, 0.0);
            pan.focus.x.abs()
        };

        assert!(travel(0.5) < travel(2.0), "缩放没影响拖动距离");
    }

    #[test]
    fn zoom_is_multiplicative_and_clamped() {
        let mut pan = PanCamera::new(10.0).with_zoom_range(0.5, 4.0);

        for _ in 0..200 {
            pan.zoom_by(1.0);
        }
        assert!(pan.zoom <= 4.0);

        for _ in 0..400 {
            pan.zoom_by(-1.0);
        }
        assert!(pan.zoom >= 0.5);
        // 缩放归零的话可视高度也归零，正交矩阵会退化。
        assert!(pan.viewport_height() > 0.0);
    }

    #[test]
    fn keyboard_travel_scales_with_zoom_too() {
        let travel = |zoom: f32| {
            let mut pan = PanCamera {
                zoom,
                ..PanCamera::new(10.0)
            };
            pan.travel(Vec2::X, 1.0);
            pan.focus.x
        };

        assert!(travel(0.5) < travel(2.0));
    }

    #[test]
    fn a_non_finite_input_is_ignored() {
        let mut pan = PanCamera::new(10.0);
        pan.travel(Vec2::X, f32::NAN);
        pan.zoom_by(f32::INFINITY);

        assert_eq!(pan.focus, Vec2::ZERO);
        assert!(pan.zoom.is_finite());
    }

    #[test]
    fn the_camera_looks_straight_at_the_plane() {
        let pan = PanCamera::new(10.0);
        let matrix = pan.transform(20.0);
        let (_, rotation, translation) = matrix.to_scale_rotation_translation();

        assert_eq!(translation, Vec3::new(0.0, 0.0, 20.0));
        // 无旋转 = 朝向 -Z = 正对 XY 平面。
        assert!((rotation.dot(Quat::IDENTITY).abs() - 1.0).abs() < 1e-5);
    }
}
