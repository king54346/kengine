//! 骨骼与动画播放器——挂在场景节点上的两个组件。
//!
//! kanim 只认识「目标序号」，因为剪辑是可共享的资源，不能记住任何一个实例的句柄。
//! 序号到句柄的映射就落在这里：[`AnimationPlayer`] 在模型实例化时建立它，
//! 每帧把 kanim 混出来的姿态写回节点的局部变换。

use crate::Node;
use kanim::{Animator, Pose};
use kcore::pool::Handle;
use kmath::Mat4;

/// 挂在蒙皮网格节点上的骨架。
#[derive(Debug, Clone)]
pub struct Skin {
    /// 关节节点。顶点里的关节号就是这个数组的下标。
    joints: Vec<Handle<Node>>,
    /// 逆绑定矩阵，与 `joints` 一一对应。
    inverse_bind: Vec<Mat4>,
    /// 每帧算出的骨骼矩阵：`关节世界变换 × 逆绑定矩阵`。
    matrices: Vec<Mat4>,
}

impl Skin {
    /// 用关节与逆绑定矩阵构造。
    ///
    /// 两者数量不一致时按较短的截断——宁可少驱动几根骨头，
    /// 也不能让顶点按错位的下标去取矩阵。
    pub fn new(joints: Vec<Handle<Node>>, inverse_bind: Vec<Mat4>) -> Self {
        let count = joints.len().min(inverse_bind.len());
        Self {
            joints: joints[..count].to_vec(),
            inverse_bind: inverse_bind[..count].to_vec(),
            matrices: vec![Mat4::IDENTITY; count],
        }
    }

    /// 关节数量。
    pub fn len(&self) -> usize {
        self.joints.len()
    }

    /// 是否没有关节。
    pub fn is_empty(&self) -> bool {
        self.joints.is_empty()
    }

    /// 关节节点。
    pub fn joints(&self) -> &[Handle<Node>] {
        &self.joints
    }

    /// 逆绑定矩阵。
    pub fn inverse_bind(&self) -> &[Mat4] {
        &self.inverse_bind
    }

    /// 上一次 [`Scene::update`](crate::Scene::update) 算出的骨骼矩阵，可直接上传显存。
    pub fn matrices(&self) -> &[Mat4] {
        &self.matrices
    }

    /// 按关节的世界变换重算骨骼矩阵。
    ///
    /// `world_transform` 传入各关节当前的世界变换（缺失的关节给 [`None`]）。
    ///
    /// 这里**不**左乘蒙皮网格节点世界变换的逆：按 glTF 规范，蒙皮网格自身节点的
    /// 变换应当被忽略，模型的整体位姿已经包含在关节的世界变换里了。
    /// 所以渲染蒙皮网格时，物体的模型矩阵取单位阵。
    pub fn update(&mut self, mut world_transform: impl FnMut(Handle<Node>) -> Option<Mat4>) {
        self.matrices.resize(self.joints.len(), Mat4::IDENTITY);
        for (index, &joint) in self.joints.iter().enumerate() {
            let global = world_transform(joint).unwrap_or(Mat4::IDENTITY);
            self.matrices[index] = global * self.inverse_bind[index];
        }
    }
}

/// 挂在模型根节点上的动画播放器。
#[derive(Debug, Clone)]
pub struct AnimationPlayer {
    animator: Animator,
    /// 目标序号（模型里的节点序号）到场景句柄的映射。
    targets: Vec<Handle<Node>>,
}

impl AnimationPlayer {
    /// 用播放器与目标映射构造。
    pub fn new(animator: Animator, targets: Vec<Handle<Node>>) -> Self {
        Self { animator, targets }
    }

    /// 底层播放器，用来选剪辑、调权重、接状态机。
    pub fn animator(&self) -> &Animator {
        &self.animator
    }

    /// 底层播放器的可变引用。
    pub fn animator_mut(&mut self) -> &mut Animator {
        &mut self.animator
    }

    /// 目标序号到场景句柄的映射。
    pub fn targets(&self) -> &[Handle<Node>] {
        &self.targets
    }

    /// 姿态里某个目标对应的场景节点。
    ///
    /// 模型里有节点没被实例化时返回 [`Handle::NONE`]，调用方按无效句柄处理即可。
    pub fn target(&self, index: usize) -> Handle<Node> {
        self.targets.get(index).copied().unwrap_or(Handle::NONE)
    }

    /// 当前姿态。
    pub fn pose(&self) -> &Pose {
        self.animator.pose()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kmath::Vec3;

    #[test]
    fn skin_truncates_mismatched_inputs() {
        // 关节比矩阵多：多出来的关节没有对应矩阵，只能丢掉，
        // 否则顶点会按错位的下标取矩阵，整个模型炸开。
        let skin = Skin::new(
            vec![Handle::new(1, 1), Handle::new(2, 1), Handle::new(3, 1)],
            vec![Mat4::IDENTITY, Mat4::IDENTITY],
        );

        assert_eq!(skin.len(), 2);
        assert_eq!(skin.inverse_bind().len(), 2);
        assert_eq!(skin.matrices().len(), 2);
    }

    #[test]
    fn skin_matrix_combines_joint_transform_and_inverse_bind() {
        // 逆绑定矩阵把顶点从模型空间搬到关节空间，关节的世界变换再把它搬回世界。
        let bind = Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0));
        let mut skin = Skin::new(vec![Handle::new(1, 1)], vec![bind.inverse()]);

        // 关节从绑定位置又往上走了 3。
        let joint_world = Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0));
        skin.update(|_| Some(joint_world));

        // 绑定姿态下位于关节处的顶点，蒙皮后应当跟到关节的新位置。
        let vertex = Vec3::new(0.0, 2.0, 0.0);
        let skinned = skin.matrices()[0].transform_point3(vertex);
        assert!((skinned - Vec3::new(0.0, 5.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn bind_pose_leaves_vertices_where_they_are() {
        // 关节停在绑定姿态时，蒙皮矩阵必须是单位阵——
        // 这是检查逆绑定矩阵方向有没有搞反最直接的办法。
        let bind = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let mut skin = Skin::new(vec![Handle::new(1, 1)], vec![bind.inverse()]);

        skin.update(|_| Some(bind));

        let vertex = Vec3::new(4.0, 5.0, 6.0);
        assert!((skin.matrices()[0].transform_point3(vertex) - vertex).length() < 1e-5);
    }

    #[test]
    fn missing_joints_fall_back_to_identity() {
        let mut skin = Skin::new(vec![Handle::NONE], vec![Mat4::IDENTITY]);

        // 关节节点被删掉了：不能 panic，给单位阵即可。
        skin.update(|_| None);

        assert_eq!(skin.matrices()[0], Mat4::IDENTITY);
    }

    #[test]
    fn empty_skin_is_harmless() {
        let mut skin = Skin::new(Vec::new(), Vec::new());

        skin.update(|_| None);

        assert!(skin.is_empty());
        assert!(skin.matrices().is_empty());
    }
}
