//! 轨道相机：绕着一个点转。
//!
//! 看模型、看场景、做演示，十有八九用的就是这个。之前每个例子都在
//! 自己那份 `update` 里手搓一遍球坐标，抄错一次符号就是「上下反了」。
//!
//! 这里**只管状态和数学**，不碰输入——鼠标怎么映射到拖动、滚轮怎么映射到
//! 缩放，各游戏的口味不同，而且 kcamera 不该认识 kinput。调用方每帧把
//! 增量喂进来即可。
//!
//! ```
//! # use kcamera::OrbitCamera;
//! # use kmath::Vec3;
//! let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0);
//!
//! orbit.rotate(0.01, 0.0);   // 拖动
//! orbit.zoom(-1.0);          // 滚轮
//! orbit.update(1.0 / 60.0);  // 自动旋转与平滑
//!
//! let transform = orbit.transform();   // 拿去设给相机节点
//! ```

use kmath::{Mat4, Quat, Vec3};

/// 一个绕着目标点转的相机。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitCamera {
    /// 看向的那个点。
    pub target: Vec3,
    /// 到目标点的距离。
    pub distance: f32,
    /// 水平角（弧度）。
    pub yaw: f32,
    /// 俯仰角（弧度）。正数是从上往下看。
    pub pitch: f32,

    /// 距离的下限。
    pub min_distance: f32,
    /// 距离的上限。
    pub max_distance: f32,
    /// 俯仰角的下限（弧度）。
    pub min_pitch: f32,
    /// 俯仰角的上限（弧度）。
    ///
    /// 默认卡在 ±89°，**不能到 ±90°**：正上方看下去时，
    /// 相机的前向和「上」向量平行，`look_at` 算出的基向量退化成 NaN，
    /// 画面整个消失。
    pub max_pitch: f32,

    /// 自动旋转的角速度（弧度/秒）。0 表示不转。
    pub auto_rotate: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self::new(Vec3::ZERO, 5.0)
    }
}

impl OrbitCamera {
    /// 俯仰角的默认极限：89°。
    ///
    /// 差的那 1° 是必须的，见 [`max_pitch`](Self::max_pitch)。
    pub const PITCH_LIMIT: f32 = 89.0 * (std::f32::consts::PI / 180.0);

    /// 建一个看着 `target`、距离 `distance` 的轨道相机。
    pub fn new(target: Vec3, distance: f32) -> Self {
        Self {
            target,
            distance,
            yaw: 0.0,
            // 略微俯视：正对着看的话，地面是一条线，什么都看不出来。
            pitch: 20.0_f32.to_radians(),
            min_distance: 0.1,
            max_distance: 1000.0,
            min_pitch: -Self::PITCH_LIMIT,
            max_pitch: Self::PITCH_LIMIT,
            auto_rotate: 0.0,
        }
    }

    /// 设置距离范围。
    pub fn with_distance_range(mut self, min: f32, max: f32) -> Self {
        self.min_distance = min.max(1e-3);
        self.max_distance = max.max(self.min_distance);
        self.distance = self.distance.clamp(self.min_distance, self.max_distance);
        self
    }

    /// 设置俯仰角范围（弧度）。会被夹进 ±89°，理由见 [`max_pitch`](Self::max_pitch)。
    pub fn with_pitch_range(mut self, min: f32, max: f32) -> Self {
        self.min_pitch = min.max(-Self::PITCH_LIMIT);
        self.max_pitch = max.min(Self::PITCH_LIMIT).max(self.min_pitch);
        self
    }

    /// 开启自动旋转，`radians_per_second` 是角速度。
    pub fn with_auto_rotate(mut self, radians_per_second: f32) -> Self {
        self.auto_rotate = radians_per_second;
        self
    }

    /// 设置初始角度（弧度）。
    pub fn with_angles(mut self, yaw: f32, pitch: f32) -> Self {
        self.yaw = yaw;
        self.pitch = pitch;
        self.clamp();
        self
    }

    /// 转动。通常接鼠标拖动的像素增量乘一个灵敏度。
    pub fn rotate(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.yaw += yaw_delta;
        self.pitch += pitch_delta;
        self.clamp();
    }

    /// 拉近拉远。`delta` 为负是拉近。
    ///
    /// 缩放量按**当前距离的比例**算，不是固定步长：离得远时一格滚轮
    /// 该跨很大一段，离得近时该很精细。固定步长的话，近处会一格就穿过去。
    pub fn zoom(&mut self, delta: f32) {
        self.distance *= (1.0 + delta * 0.1).clamp(0.1, 10.0);
        self.clamp();
    }

    /// 平移目标点。通常接鼠标中键或右键拖动。
    ///
    /// 平移方向跟着相机走：屏幕上往右拖，目标就往相机的右方移动。
    pub fn pan(&mut self, right: f32, up: f32) {
        let rotation = self.rotation();
        // 按距离缩放：离得远时同样的拖动该移动更多，否则大场景里挪不动。
        let scale = self.distance * 0.001;
        self.target += (rotation * Vec3::X) * right * scale;
        self.target += (rotation * Vec3::Y) * up * scale;
    }

    /// 推进一帧：处理自动旋转。
    pub fn update(&mut self, dt: f32) {
        if self.auto_rotate != 0.0 && dt.is_finite() {
            self.yaw += self.auto_rotate * dt;
        }
    }

    /// 相机在世界空间的位置。
    pub fn position(&self) -> Vec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();

        // 球坐标：pitch 为正时相机在目标上方。
        let offset = Vec3::new(cos_pitch * sin_yaw, sin_pitch, cos_pitch * cos_yaw) * self.distance;

        self.target + offset
    }

    /// 相机的朝向。
    pub fn rotation(&self) -> Quat {
        // 相机看向自己的 -Z（glTF 的约定），所以局部 +Z 是「背后」。
        let back = (self.position() - self.target).normalize_or(Vec3::Z);
        // 俯仰被夹在 ±89°，所以 back 和 Y 轴不会平行，基向量不会退化。
        let right = Vec3::Y.cross(back).normalize_or(Vec3::X);
        let up = back.cross(right);

        // 顺序必须让 `right × up == back`，也就是行列式为 +1。
        // 弄成 -1 的话这是个**反射**而不是旋转，`from_mat3` 会给出
        // 非单位四元数——转出来的向量长度只有 0.5，画面缩成一团，
        // 而且不报任何错。
        Quat::from_mat3(&kmath::Mat3::from_cols(right, up, back))
    }

    /// 相机的世界变换，直接设给节点。
    pub fn transform(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation(), self.position())
    }

    fn clamp(&mut self) {
        self.distance = self
            .distance
            .clamp(self.min_distance.max(1e-3), self.max_distance);
        self.pitch = self.pitch.clamp(self.min_pitch, self.max_pitch);
        // yaw 绕回主区间，免得一直转下去之后浮点精度变差——
        // 转上几个小时，f32 在 1e6 弧度上的分辨率会粗到肉眼可见的跳动。
        self.yaw = self.yaw.rem_euclid(std::f32::consts::TAU);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn the_camera_sits_at_the_requested_distance() {
        let orbit = OrbitCamera::new(Vec3::new(1.0, 2.0, 3.0), 7.0);
        let offset = orbit.position() - orbit.target;
        assert!(
            (offset.length() - 7.0).abs() < 1e-4,
            "距离是 {}",
            offset.length()
        );
    }

    #[test]
    fn the_camera_looks_at_the_target() {
        let orbit = OrbitCamera::new(Vec3::new(0.0, 1.0, 0.0), 5.0);
        // 相机看向自己的 -Z。
        let forward = orbit.rotation() * Vec3::NEG_Z;
        let to_target = (orbit.target - orbit.position()).normalize();
        assert!(
            (forward - to_target).length() < 1e-4,
            "朝向对不上：{forward:?} vs {to_target:?}"
        );
    }

    #[test]
    fn a_positive_pitch_puts_the_camera_above() {
        // 符号反了的话「往上拖」会变成从地下往上看，一眼就错但很容易写反。
        let orbit = OrbitCamera::new(Vec3::ZERO, 5.0).with_angles(0.0, 30.0_f32.to_radians());
        assert!(orbit.position().y > 0.0, "俯仰为正却在下面");
    }

    #[test]
    fn pitch_is_clamped_short_of_the_pole() {
        // 到了正上方，前向和「上」平行，基向量退化成 NaN，画面整个消失。
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0);
        orbit.rotate(0.0, 100.0);
        assert!(orbit.pitch <= OrbitCamera::PITCH_LIMIT);
        assert!(orbit.rotation().is_finite(), "极点处朝向变成了 NaN");

        orbit.rotate(0.0, -1000.0);
        assert!(orbit.pitch >= -OrbitCamera::PITCH_LIMIT);
        assert!(orbit.rotation().is_finite());
    }

    #[test]
    fn the_transform_is_never_degenerate() {
        // 扫一圈所有角度，任何一个位姿出 NaN 都会让那一帧的画面全黑。
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0);
        for i in 0..360 {
            orbit.yaw = (i as f32).to_radians();
            for step in -9..=9 {
                orbit.pitch = (step as f32 * 10.0).to_radians();
                orbit.clamp();
                let transform = orbit.transform();
                assert!(
                    transform.to_cols_array().iter().all(|v| v.is_finite()),
                    "yaw={} pitch={} 得到 {transform:?}",
                    orbit.yaw,
                    orbit.pitch
                );
            }
        }
    }

    #[test]
    fn zoom_is_proportional_not_fixed_step() {
        // 固定步长的话，近处一格滚轮就穿过目标了。
        let mut far = OrbitCamera::new(Vec3::ZERO, 100.0);
        let mut near = OrbitCamera::new(Vec3::ZERO, 1.0);
        let (far_before, near_before) = (far.distance, near.distance);

        far.zoom(-1.0);
        near.zoom(-1.0);

        let far_step = far_before - far.distance;
        let near_step = near_before - near.distance;
        assert!(
            far_step > near_step * 10.0,
            "远处的步长该大得多：{far_step} vs {near_step}"
        );
    }

    #[test]
    fn zoom_respects_its_range() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0).with_distance_range(1.0, 10.0);
        for _ in 0..200 {
            orbit.zoom(-1.0);
        }
        assert!(orbit.distance >= 1.0 - 1e-6, "缩过头了：{}", orbit.distance);

        for _ in 0..200 {
            orbit.zoom(1.0);
        }
        assert!(orbit.distance <= 10.0 + 1e-6);
    }

    #[test]
    fn zoom_never_reaches_zero() {
        // 距离为零时相机和目标重合，朝向没有定义。
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0).with_distance_range(0.0, 100.0);
        for _ in 0..1000 {
            orbit.zoom(-1.0);
        }
        assert!(orbit.distance > 0.0);
        assert!(orbit.rotation().is_finite());
    }

    #[test]
    fn auto_rotate_advances_the_yaw() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0).with_auto_rotate(1.0);
        let before = orbit.yaw;
        orbit.update(0.5);
        assert!((orbit.yaw - before - 0.5).abs() < 1e-5);
    }

    #[test]
    fn auto_rotate_off_by_default() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0);
        let before = orbit.yaw;
        orbit.update(1.0);
        assert_eq!(orbit.yaw, before);
    }

    #[test]
    fn a_bogus_delta_does_not_corrupt_the_yaw() {
        // 第一帧的 dt 有时是 NaN 或 inf（时钟还没初始化）。
        // 让 yaw 变成 NaN 的话，之后每一帧的画面都是黑的，而且再也回不来。
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0).with_auto_rotate(1.0);
        orbit.update(f32::NAN);
        orbit.update(f32::INFINITY);
        assert!(orbit.yaw.is_finite(), "yaw 被污染成了 {}", orbit.yaw);
    }

    #[test]
    fn the_yaw_stays_in_the_principal_range() {
        // 转上几个小时之后，f32 在 1e6 弧度上的分辨率会粗到肉眼可见的跳动。
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 5.0);
        for _ in 0..10_000 {
            orbit.rotate(1.0, 0.0);
        }
        assert!(
            orbit.yaw.abs() <= std::f32::consts::TAU + 1e-4,
            "{}",
            orbit.yaw
        );
    }

    #[test]
    fn panning_moves_along_the_camera_axes() {
        // 屏幕上往右拖，目标该往相机的右方移动，不是世界的 +X。
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0).with_angles(90.0_f32.to_radians(), 0.0);
        let right = orbit.rotation() * Vec3::X;

        orbit.pan(100.0, 0.0);
        let moved = orbit.target.normalize_or_zero();

        assert!(
            moved.dot(right) > 0.9,
            "平移方向不跟着相机走：{moved:?} vs {right:?}"
        );
    }

    #[test]
    fn panning_scales_with_distance() {
        // 大场景里离得远还按固定步长挪的话，根本挪不动。
        let mut close = OrbitCamera::new(Vec3::ZERO, 1.0);
        let mut far = OrbitCamera::new(Vec3::ZERO, 100.0);
        close.pan(100.0, 0.0);
        far.pan(100.0, 0.0);

        assert!(far.target.length() > close.target.length() * 10.0);
    }
}
