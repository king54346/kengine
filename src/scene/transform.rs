//! 节点的局部变换。

use kmath::{Mat4, Quat, Vec3};

/// 位置 / 旋转 / 缩放三元组，可组合成一个局部变换矩阵。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// 相对父节点的位置。
    pub position: Vec3,
    /// 相对父节点的旋转。
    pub rotation: Quat,
    /// 相对父节点的缩放。
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// 不做任何变换的单位变换。
    pub const IDENTITY: Self = Self {
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// 仅指定位置。
    pub fn from_position(position: Vec3) -> Self {
        Self {
            position,
            ..Self::IDENTITY
        }
    }

    /// 构造一个位于 `eye`、朝向 `target` 的变换，常用于相机节点。
    pub fn looking_at(eye: Vec3, target: Vec3, up: Vec3) -> Self {
        // `look_at` 得到的是世界 → 观察空间的矩阵，取逆才是该节点在世界中的位姿。
        let camera_to_world = Mat4::look_at_rh(eye, target, up).inverse();
        let (scale, rotation, position) = camera_to_world.to_scale_rotation_translation();
        Self {
            position,
            rotation,
            scale,
        }
    }

    /// 组合成局部变换矩阵。
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.position)
    }

    /// 绕自身 X 轴追加旋转（弧度）。
    pub fn rotate_x(&mut self, angle: f32) {
        self.rotation *= Quat::from_rotation_x(angle);
    }

    /// 绕自身 Y 轴追加旋转（弧度）。
    pub fn rotate_y(&mut self, angle: f32) {
        self.rotation *= Quat::from_rotation_y(angle);
    }

    /// 绕自身 Z 轴追加旋转（弧度）。
    pub fn rotate_z(&mut self, angle: f32) {
        self.rotation *= Quat::from_rotation_z(angle);
    }

    /// 在世界空间中平移。
    pub fn translate(&mut self, offset: Vec3) {
        self.position += offset;
    }
}
