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
    /// 从一个 4×4 矩阵分解出 TRS。
    ///
    /// 拿到一个现成的世界矩阵（轨道相机、IK 解算、外部工具算出来的）
    /// 想设给节点时用。
    ///
    /// # 分解不是无损的
    ///
    /// 只有「缩放 → 旋转 → 平移」这种顺序拼出来的矩阵才能精确还原。
    /// 带切变的矩阵（非均匀缩放之后又旋转过）分解出的旋转是个近似，
    /// 再拼回去和原矩阵不相等。
    pub fn from_matrix(matrix: Mat4) -> Self {
        let (scale, rotation, position) = matrix.to_scale_rotation_translation();
        Self {
            position,
            rotation,
            scale,
        }
    }

    /// 组合成 4×4 变换矩阵（缩放 → 旋转 → 平移）。
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn from_matrix_round_trips_a_trs_matrix() {
        let original = Transform {
            position: Vec3::new(1.0, -2.0, 3.0),
            rotation: Quat::from_rotation_y(0.7),
            scale: Vec3::splat(2.0),
        };
        let back = Transform::from_matrix(original.matrix());

        assert!((back.position - original.position).length() < 1e-5);
        assert!((back.scale - original.scale).length() < 1e-5);
        // 四元数可能差一个整体符号，比的是它转出来的向量。
        assert!(((back.rotation * Vec3::X) - (original.rotation * Vec3::X)).length() < 1e-5);
    }

    #[test]
    fn from_matrix_survives_a_degenerate_matrix() {
        // 零矩阵分解出的缩放是零、旋转没有定义。不该产生 NaN——
        // NaN 位置会让那个节点连同它的整棵子树从画面上消失。
        let transform = Transform::from_matrix(Mat4::ZERO);
        assert!(transform.position.is_finite());
        assert!(transform.scale.is_finite());
    }
}
