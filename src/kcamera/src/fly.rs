//! 自由飞行相机：WASD 加鼠标看，像大多数编辑器那样。
//!
//! 和 [`OrbitCamera`](crate::OrbitCamera) 的分工很清楚：轨道相机绕着**一个
//! 目标**转，适合看东西；这个没有目标，走到哪算哪，适合在场景里逛。
//! 调试关卡、找穿帮、录演示镜头用它。
//!
//! 同样**只管状态和数学，不碰输入**——鼠标怎么映射到转头、哪个键是加速，
//! 各游戏口味不同，而且 kcamera 不该认识 kinput。
//!
//! ```
//! # use kcamera::FlyCamera;
//! # use kmath::Vec3;
//! let mut fly = FlyCamera::new(Vec3::new(0.0, 2.0, 10.0));
//!
//! fly.look(-0.01, 0.0);                       // 鼠标左右
//! fly.travel(Vec3::new(0.0, 0.0, 1.0), 0.016); // 按住 W
//!
//! let transform = fly.transform();             // 拿去设给相机节点
//! ```

use kmath::{Mat4, Quat, Vec3};

/// 一台自由飞行的相机。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlyCamera {
    /// 当前位置。
    pub position: Vec3,
    /// 水平角（弧度）。
    pub yaw: f32,
    /// 俯仰角（弧度）。
    pub pitch: f32,
    /// 俯仰下限。
    pub min_pitch: f32,
    /// 俯仰上限。
    ///
    /// **必须夹住**：到了正上方，视线和世界的上方向共线，用来建基向量的
    /// 叉积会退化成零向量，画面猛地翻一下。默认留了约 1° 的余量。
    pub max_pitch: f32,
    /// 移动速度（世界单位／秒）。
    pub speed: f32,
    /// 加速档的倍率。
    pub boost: f32,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            min_pitch: -std::f32::consts::FRAC_PI_2 + 0.02,
            max_pitch: std::f32::consts::FRAC_PI_2 - 0.02,
            speed: 6.0,
            boost: 4.0,
        }
    }
}

impl FlyCamera {
    /// 从一个位置起飞，朝向 -Z。
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            ..Self::default()
        }
    }

    /// 设定初始朝向。
    pub fn with_angles(mut self, yaw: f32, pitch: f32) -> Self {
        self.yaw = yaw;
        self.pitch = pitch;
        self.clamp();
        self
    }

    /// 设定移动速度。
    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    /// 转头。通常接鼠标位移的像素增量乘一个灵敏度。
    ///
    /// **别乘 `dt`**：鼠标位移本身就是「上一帧到这一帧移动了多少」，
    /// 再乘一次时间会让帧率越高转得越慢。
    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.yaw += yaw_delta;
        self.pitch += pitch_delta;
        self.clamp();
    }

    /// 移动。`axes` 的三个分量分别是右、上、前，取值 `-1..=1`。
    ///
    /// 前后左右跟着视线走，**上下用世界的 Y**：低头按「上升」时，人要的是
    /// 垂直升高，不是沿着视线斜着钻进地里。
    pub fn travel(&mut self, axes: Vec3, dt: f32) {
        self.travel_with(axes, dt, false);
    }

    /// 带加速档的移动。
    pub fn travel_with(&mut self, axes: Vec3, dt: f32, boosting: bool) {
        if !dt.is_finite() {
            return;
        }
        let rotation = self.rotation();
        let forward = rotation * Vec3::NEG_Z;
        let right = rotation * Vec3::X;

        let speed = if boosting {
            self.speed * self.boost
        } else {
            self.speed
        } * dt;

        self.position += forward * (axes.z * speed);
        self.position += right * (axes.x * speed);
        self.position += Vec3::Y * (axes.y * speed);
    }

    /// 相机的朝向。
    ///
    /// 顺序是 **YXZ**：先绕世界 Y 转（左右看），再绕自身 X 转（上下看）。
    /// 反过来的话，抬头之后左右转会绕着歪掉的轴走，画面跟着倾斜——
    /// 第一人称视角里这种「万向节感」尤其难受。
    pub fn rotation(&self) -> Quat {
        Quat::from_euler(kmath::EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }

    /// 相机的世界变换，直接设给节点。
    pub fn transform(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation(), self.position)
    }

    fn clamp(&mut self) {
        self.pitch = self.pitch.clamp(self.min_pitch, self.max_pitch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_moves_where_it_looks() {
        let mut fly = FlyCamera::default();
        fly.travel(Vec3::new(0.0, 0.0, 1.0), 1.0);

        // 默认朝向 -Z。
        assert!(fly.position.z < -1.0, "没往前飞：{:?}", fly.position);
        assert!(fly.position.x.abs() < 1e-5);
    }

    #[test]
    fn turning_changes_where_forward_is() {
        let mut fly = FlyCamera::default();
        fly.look(-std::f32::consts::FRAC_PI_2, 0.0);
        fly.travel(Vec3::new(0.0, 0.0, 1.0), 1.0);

        // 右转 90° 之后，「前」变成了世界的 +X。
        assert!(fly.position.x > 1.0, "转向没生效：{:?}", fly.position);
        assert!(fly.position.z.abs() < 1e-4);
    }

    #[test]
    fn going_up_is_vertical_even_when_looking_down() {
        // 上下用世界 Y：低头按「上升」时人要的是垂直升高，
        // 不是沿视线斜着钻进地里。
        let mut fly = FlyCamera::default().with_angles(0.0, -1.0);
        fly.travel(Vec3::Y, 1.0);

        assert!(fly.position.y > 0.0);
        assert!(
            fly.position.z.abs() < 1e-4,
            "上升带出了水平位移：{:?}",
            fly.position
        );
    }

    #[test]
    fn pitch_is_clamped_at_the_poles() {
        // 到了正上方，视线与世界上方共线，建基向量的叉积会退化。
        let mut fly = FlyCamera::default();
        for _ in 0..1000 {
            fly.look(0.0, 1.0);
        }

        assert!(fly.pitch <= fly.max_pitch);
        assert!(fly.pitch < std::f32::consts::FRAC_PI_2);
        assert!(fly.rotation().is_finite());
        assert!((fly.rotation().length() - 1.0).abs() < 1e-4, "四元数没归一化");
    }

    #[test]
    fn boost_is_faster() {
        let travel = |boosting: bool| {
            let mut fly = FlyCamera::default();
            fly.travel_with(Vec3::Z, 1.0, boosting);
            fly.position.length()
        };

        assert!(travel(true) > travel(false) * 3.0);
    }

    #[test]
    fn a_non_finite_dt_does_not_teleport_the_camera() {
        // 第一帧的 dt 有时会是 0 或者奇怪的值；NaN 一旦进了位置，
        // 相机就再也回不来了，而且画面直接空白。
        let mut fly = FlyCamera::new(Vec3::Y);
        fly.travel(Vec3::Z, f32::NAN);

        assert_eq!(fly.position, Vec3::Y);
    }

    #[test]
    fn the_transform_round_trips_the_pose() {
        let fly = FlyCamera::new(Vec3::new(1.0, 2.0, 3.0)).with_angles(0.5, -0.3);
        let matrix = fly.transform();

        let (_, rotation, translation) = matrix.to_scale_rotation_translation();
        assert!((translation - fly.position).length() < 1e-5);
        assert!((rotation.dot(fly.rotation()).abs() - 1.0).abs() < 1e-4);
    }
}
