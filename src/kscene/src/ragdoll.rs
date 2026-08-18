//! 布娃娃：一组用关节连起来的刚体，反过来驱动骨骼。
//!
//! # 两个方向
//!
//! 布娃娃只有两种状态，方向正好相反：
//!
//! - **未激活**：骨骼（动画）驱动刚体。刚体切成运动学，每帧被摆到骨骼所在的位置。
//!   角色正常走路时就是这个状态，物理只是「跟着看」。
//! - **激活**：刚体（物理）驱动骨骼。刚体切回动态，模拟结果写进骨骼的局部变换。
//!   角色倒下、被击飞时切到这里。
//!
//! 未激活时也要老老实实跟着骨骼摆，是为了**激活那一刻不穿帮**——
//! 刚体如果停在几秒前的位置，切换瞬间角色会从那里「闪现」过来。
//!
//! # 层级
//!
//! 肢体刚体挂在布娃娃节点下，与骨骼树是**两棵独立的树**（Fyrox 也是这么分的）。
//! 混在一起的话，激活时骨骼的局部变换正被物理改写，而刚体的世界位姿又依赖
//! 骨骼的世界变换，两者会互相追着跑。

use crate::{Collider, Joint, Node, RigidBody, Scene, Transform};
use kcore::pool::Handle;
use kmath::{Quat, Vec3};
use kphysics::{ColliderDesc, ColliderShape, JointDesc, RigidBodyType};

/// 布娃娃的一节肢体：一根骨骼 + 一个刚体 + 若干子肢体。
#[derive(Debug, Clone, PartialEq)]
pub struct RagdollLimb {
    /// 被驱动的骨骼节点（蒙皮网格的关节之一）。
    pub bone: Handle<Node>,
    /// 对应的刚体节点。
    pub body: Handle<Node>,
    /// 子肢体。
    pub children: Vec<RagdollLimb>,
}

impl RagdollLimb {
    /// 深度优先遍历，父肢体总在子肢体之前。
    ///
    /// 顺序是有意义的：驱动骨骼时子骨骼的局部变换要用父骨骼**已经更新过**的
    /// 世界变换来换算，反过来遍历会慢一帧。
    pub fn for_each<F: FnMut(&RagdollLimb)>(&self, f: &mut F) {
        f(self);
        for child in &self.children {
            child.for_each(f);
        }
    }
}

/// 布娃娃组件。
#[derive(Debug, Clone)]
pub struct Ragdoll {
    root: RagdollLimb,
    active: bool,
    /// 上一帧的状态，用来识别「刚刚激活」这一瞬间。
    prev_active: bool,
    /// 角色本体的刚体节点。激活时它会被切成运动学并跟着根肢体走，
    /// 否则角色胶囊会和布娃娃抢位置，两边互相推。
    character_body: Handle<Node>,
    /// 角色刚体在激活前的类型，退出布娃娃时要还回去。
    character_body_type: Option<RigidBodyType>,
}

impl Ragdoll {
    /// 用给定的肢体树创建，默认未激活。
    pub fn new(root: RagdollLimb) -> Self {
        Self {
            root,
            active: false,
            prev_active: false,
            character_body: Handle::NONE,
            character_body_type: None,
        }
    }

    /// 指定角色本体的刚体节点。
    pub fn with_character_body(mut self, body: Handle<Node>) -> Self {
        self.character_body = body;
        self
    }

    /// 根肢体。
    pub fn root(&self) -> &RagdollLimb {
        &self.root
    }

    /// 是否处于激活（物理驱动）状态。
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 激活 / 关闭布娃娃。
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// 肢体数量。
    pub fn limb_count(&self) -> usize {
        let mut count = 0;
        self.root.for_each(&mut |_| count += 1);
        count
    }
}

/// 一节肢体的构建参数。
#[derive(Debug, Clone)]
pub struct LimbDesc {
    /// 要驱动的骨骼节点。
    pub bone: Handle<Node>,
    /// 碰撞体形状，局部空间。
    pub shape: ColliderShape,
    /// 碰撞体相对骨骼的偏移。胶囊通常要沿骨头方向挪半个长度。
    pub offset: Vec3,
    /// 碰撞体相对骨骼的旋转。
    pub rotation: Quat,
    /// 与父肢体之间的关节。根肢体填 `None`。
    ///
    /// 两端的锚点会被**自动覆盖**成「这根骨骼的原点」——那正是关节该在的地方。
    /// 类型与限位（`kind`、`local_basis*`）保持你给的值，那才是真正要调的部分。
    pub joint: Option<JointDesc>,
    /// 子肢体。
    pub children: Vec<LimbDesc>,
}

impl LimbDesc {
    /// 用一个球形碰撞体描述一节肢体。
    pub fn new(bone: Handle<Node>, shape: ColliderShape) -> Self {
        Self {
            bone,
            shape,
            offset: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            joint: None,
            children: Vec::new(),
        }
    }

    /// 指定碰撞体相对骨骼的偏移与旋转。
    pub fn with_offset(mut self, offset: Vec3, rotation: Quat) -> Self {
        self.offset = offset;
        self.rotation = rotation;
        self
    }

    /// 指定与父肢体之间的关节。
    pub fn with_joint(mut self, joint: JointDesc) -> Self {
        self.joint = Some(joint);
        self
    }

    /// 挂一节子肢体。
    pub fn with_child(mut self, child: LimbDesc) -> Self {
        self.children.push(child);
        self
    }
}

/// 按肢体描述在场景里搭出一整套布娃娃。
#[derive(Debug, Clone)]
pub struct RagdollBuilder {
    root: LimbDesc,
    name: String,
    character_body: Handle<Node>,
    mass: f32,
}

impl RagdollBuilder {
    /// 从根肢体开始。
    pub fn new(root: LimbDesc) -> Self {
        Self {
            root,
            name: "Ragdoll".to_string(),
            character_body: Handle::NONE,
            mass: 0.0,
        }
    }

    /// 指定布娃娃节点的名字。
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// 指定角色本体的刚体节点。
    pub fn with_character_body(mut self, body: Handle<Node>) -> Self {
        self.character_body = body;
        self
    }

    /// 给每节肢体追加的质量。0 表示由碰撞体的密度自行决定。
    pub fn with_limb_mass(mut self, mass: f32) -> Self {
        self.mass = mass;
        self
    }

    /// 在场景里建出所有刚体、碰撞体与关节，返回布娃娃节点。
    ///
    /// 刚体的初始位姿取自各自骨骼**当前**的世界变换——所以调用之前
    /// 骨架应该处在一个合理的姿态（比如绑定姿态或动画的某一帧）。
    pub fn build(self, scene: &mut Scene, parent: Handle<Node>) -> Handle<Node> {
        let ragdoll_node = scene.add_node_with_parent(Node::new(self.name.clone()), parent);

        let root_limb = Self::build_limb(scene, &self.root, ragdoll_node, Handle::NONE, self.mass);

        let mut ragdoll = Ragdoll::new(root_limb);
        if self.character_body.is_some() {
            ragdoll = ragdoll.with_character_body(self.character_body);
        }
        if let Some(node) = scene.try_get_mut(ragdoll_node) {
            node.set_ragdoll(ragdoll);
        }
        scene.reindex_physics(ragdoll_node);

        ragdoll_node
    }

    fn build_limb(
        scene: &mut Scene,
        desc: &LimbDesc,
        container: Handle<Node>,
        parent_body: Handle<Node>,
        mass: f32,
    ) -> RagdollLimb {
        // 刚体节点直接放在骨骼当前所在的位姿上。
        let bone_world = scene.world_matrix(desc.bone);
        let (position, rotation) = kphysics::pose_from_matrix(bone_world);

        let mut body_desc = kphysics::RigidBodyDesc::dynamic();
        body_desc.additional_mass = mass;
        // 布娃娃的肢体一睡着就再也醒不过来（关节的拉扯不足以唤醒），
        // 看起来就像半空中卡住了。
        body_desc.can_sleep = false;

        let body_node = scene.add_node_with_parent(
            Node::new(format!("Limb:{}", desc.bone.index()))
                .with_transform(Transform {
                    position,
                    rotation,
                    scale: Vec3::ONE,
                })
                .with_rigid_body(RigidBody::new(body_desc))
                .with_collider(Collider::new(
                    ColliderDesc::new(desc.shape.clone()).with_offset(desc.offset, desc.rotation),
                )),
            container,
        );

        if parent_body.is_some() {
            if let Some(mut joint_desc) = desc.joint.clone() {
                // 关节点就是这根骨骼的原点：在子刚体的局部空间里是原点本身，
                // 在父刚体的局部空间里要换算一次。
                let parent_world = scene.world_matrix(parent_body);
                joint_desc.local_anchor1 = parent_world.inverse().transform_point3(position);
                joint_desc.local_anchor2 = Vec3::ZERO;

                scene.add_node_with_parent(
                    Node::new("Joint").with_joint(Joint::new(parent_body, body_node, joint_desc)),
                    container,
                );
            }
        }

        RagdollLimb {
            bone: desc.bone,
            body: body_node,
            children: desc
                .children
                .iter()
                .map(|child| Self::build_limb(scene, child, container, body_node, mass))
                .collect(),
        }
    }
}

impl Scene {
    /// 物理步进**之前**：未激活的布娃娃把刚体摆到骨骼所在的位置。
    pub(crate) fn ragdolls_follow_bones(&mut self) {
        for position in 0..self.ragdoll_handles().len() {
            let handle = self.ragdoll_handles()[position];
            let Some(mut ragdoll) = self.take_ragdoll(handle) else {
                continue;
            };

            if !ragdoll.active {
                let container_world = self.world_matrix(handle);
                let inverse = container_world.inverse();

                let mut limbs = Vec::new();
                ragdoll.root.for_each(&mut |limb| limbs.push((limb.bone, limb.body)));

                for (bone, body) in limbs {
                    let bone_world = self.world_matrix(bone);
                    let (p, r) = kphysics::pose_from_matrix(inverse * bone_world);
                    if let Some(node) = self.try_get_mut(body) {
                        node.transform.position = p;
                        node.transform.rotation = r;
                        if let Some(rb) = node.rigid_body_mut() {
                            // 运动学：跟着骨骼走，不受重力，也不被别的物体推动。
                            rb.set_body_type(RigidBodyType::KinematicPositionBased);
                            rb.set_linvel(Vec3::ZERO);
                            rb.set_angvel(Vec3::ZERO);
                        }
                    }
                }

                self.restore_character_body(&mut ragdoll);
            }

            self.put_ragdoll(handle, ragdoll);
        }
    }

    /// 物理步进**之后**：激活的布娃娃把模拟结果写进骨骼。
    pub(crate) fn ragdolls_drive_bones(&mut self) {
        for position in 0..self.ragdoll_handles().len() {
            let handle = self.ragdoll_handles()[position];
            let Some(mut ragdoll) = self.take_ragdoll(handle) else {
                continue;
            };

            if ragdoll.active {
                // 刚激活的这一帧，把角色本体的速度分给各个肢体，
                // 否则奔跑中的角色会原地软倒，看不出惯性。
                let inherited = (!ragdoll.prev_active)
                    .then(|| {
                        self.try_get(ragdoll.character_body)
                            .and_then(|n| n.rigid_body())
                            .map(|b| (b.linvel(), b.angvel()))
                    })
                    .flatten();

                let mut limbs = Vec::new();
                ragdoll.root.for_each(&mut |limb| limbs.push((limb.bone, limb.body)));

                for (bone, body) in limbs {
                    if let Some(node) = self.try_get_mut(body)
                        && let Some(rb) = node.rigid_body_mut()
                    {
                        rb.set_body_type(RigidBodyType::Dynamic);
                        if let Some((linvel, angvel)) = inherited {
                            rb.set_linvel(linvel);
                            rb.set_angvel(angvel);
                        }
                    }

                    // 骨骼的局部变换 = 父骨骼世界变换的逆 × 刚体的世界位姿。
                    // 父先于子处理，`world_matrix` 读到的父骨骼已经是新的了。
                    let body_world = self.world_matrix(body);
                    let bone_parent = self.try_get(bone).map(|n| n.parent()).unwrap_or(Handle::NONE);
                    let local = if bone_parent.is_none() {
                        body_world
                    } else {
                        self.world_matrix(bone_parent).inverse() * body_world
                    };
                    let (p, r) = kphysics::pose_from_matrix(local);
                    if let Some(node) = self.try_get_mut(bone) {
                        node.transform.position = p;
                        node.transform.rotation = r;
                    }
                }

                self.take_over_character_body(&mut ragdoll);
            }

            ragdoll.prev_active = ragdoll.active;
            self.put_ragdoll(handle, ragdoll);
        }
    }

    /// 激活时接管角色本体：切成运动学并跟着根肢体，避免两套碰撞体互相推。
    fn take_over_character_body(&mut self, ragdoll: &mut Ragdoll) {
        if ragdoll.character_body.is_none() {
            return;
        }
        let root_position = self.world_matrix(ragdoll.root.body).w_axis.truncate();
        let Some(node) = self.try_get_mut(ragdoll.character_body) else {
            return;
        };
        let Some(body) = node.rigid_body_mut() else {
            return;
        };
        if ragdoll.character_body_type.is_none() {
            ragdoll.character_body_type = Some(body.body_type());
        }
        body.set_body_type(RigidBodyType::KinematicPositionBased);
        body.set_linvel(Vec3::ZERO);
        body.set_angvel(Vec3::ZERO);
        node.transform.position = root_position;
    }

    /// 退出布娃娃时把角色本体的刚体类型还回去。
    fn restore_character_body(&mut self, ragdoll: &mut Ragdoll) {
        let Some(previous) = ragdoll.character_body_type.take() else {
            return;
        };
        if let Some(node) = self.try_get_mut(ragdoll.character_body)
            && let Some(body) = node.rigid_body_mut()
        {
            body.set_body_type(previous);
        }
    }
}

/// 常用的关节限位：一个只朝单方向弯的铰链（肘、膝）。
pub fn hinge_limits(min: f32, max: f32) -> Option<[f32; 2]> {
    Some([min.min(max), min.max(max)])
}

#[cfg(test)]
mod test {
    use super::*;
    use kphysics::{JointKind, SphericalLimits};

    /// 搭一条「髋 → 大腿 → 小腿」的三节链，骨骼沿 -Y 依次向下。
    fn build_leg(scene: &mut Scene) -> (Handle<Node>, [Handle<Node>; 3]) {
        let hip = scene.add_node(Node::new("hip").with_position(Vec3::new(0.0, 3.0, 0.0)));
        let thigh = scene.add_node_with_parent(
            Node::new("thigh").with_position(Vec3::new(0.0, -0.5, 0.0)),
            hip,
        );
        let shin = scene.add_node_with_parent(
            Node::new("shin").with_position(Vec3::new(0.0, -0.5, 0.0)),
            thigh,
        );
        scene.update();

        let desc = LimbDesc::new(hip, ColliderShape::capsule_y(0.15, 0.12))
            .with_child(
                LimbDesc::new(thigh, ColliderShape::capsule_y(0.15, 0.1))
                    .with_joint(JointDesc {
                        kind: JointKind::Spherical {
                            limits: SphericalLimits::symmetric(0.6),
                        },
                        ..Default::default()
                    })
                    .with_child(LimbDesc::new(shin, ColliderShape::capsule_y(0.15, 0.09)).with_joint(
                        JointDesc {
                            kind: JointKind::Revolute {
                                axis: Vec3::X,
                                limits: hinge_limits(-2.0, 0.0),
                            },
                            ..Default::default()
                        },
                    )),
            );

        let ragdoll = RagdollBuilder::new(desc).build(scene, scene.root());
        (ragdoll, [hip, thigh, shin])
    }

    #[test]
    fn the_builder_creates_one_body_per_limb_and_one_joint_per_link() {
        let mut scene = Scene::new();
        let (ragdoll, _) = build_leg(&mut scene);
        scene.update();

        let limbs = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().limb_count();
        assert_eq!(limbs, 3);

        // 三节肢体 → 三个刚体、三个碰撞体、两个关节（根肢体没有父）。
        scene.step_physics(1.0 / 60.0);
        assert_eq!(scene.physics().body_count(), 3);
        assert_eq!(scene.physics().collider_count(), 3);
        assert_eq!(scene.physics().joint_count(), 2);
    }

    #[test]
    fn limb_bodies_start_at_their_bones() {
        let mut scene = Scene::new();
        let (ragdoll, bones) = build_leg(&mut scene);

        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let mut pairs = Vec::new();
        root.for_each(&mut |limb| pairs.push((limb.bone, limb.body)));

        for (bone, body) in pairs {
            let bone_world = scene.world_matrix(bone).w_axis.truncate();
            let body_world = scene.world_matrix(body).w_axis.truncate();
            assert!(
                (bone_world - body_world).length() < 1e-4,
                "肢体 {body:?} 没有对齐骨骼 {bone:?}"
            );
        }
        assert_eq!(bones.len(), 3);
    }

    #[test]
    fn an_inactive_ragdoll_follows_the_bones_instead_of_falling() {
        let mut scene = Scene::new();
        let (ragdoll, bones) = build_leg(&mut scene);

        for _ in 0..60 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        // 骨骼没动过，未激活的布娃娃也就不该动。
        let hip = scene.try_get(bones[0]).unwrap().transform.position;
        assert!((hip - Vec3::new(0.0, 3.0, 0.0)).length() < 1e-4, "骨骼被物理拽跑了：{hip:?}");

        let _ = ragdoll;
    }

    #[test]
    fn an_inactive_ragdoll_keeps_its_bodies_glued_to_moving_bones() {
        // 激活那一刻不能穿帮，前提就是未激活时刚体一直贴着骨骼。
        let mut scene = Scene::new();
        let (ragdoll, bones) = build_leg(&mut scene);

        for i in 0..60 {
            scene.try_get_mut(bones[0]).unwrap().transform.position =
                Vec3::new(i as f32 * 0.05, 3.0, 0.0);
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let bone_world = scene.world_matrix(root.bone).w_axis.truncate();
        let body_world = scene.world_matrix(root.body).w_axis.truncate();

        assert!(
            (bone_world - body_world).length() < 0.05,
            "骨骼在 {bone_world:?}，刚体却在 {body_world:?}"
        );
    }

    #[test]
    fn an_active_ragdoll_drives_the_bones_downward() {
        let mut scene = Scene::new();
        let (ragdoll, bones) = build_leg(&mut scene);

        scene
            .try_get_mut(ragdoll)
            .unwrap()
            .ragdoll_mut()
            .unwrap()
            .set_active(true);

        for _ in 0..90 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let hip_world = scene.world_matrix(bones[0]).w_axis.truncate();
        assert!(hip_world.y < 2.0, "激活后骨骼没有掉下去：{hip_world:?}");
    }

    #[test]
    fn joints_keep_the_limbs_connected_while_falling() {
        let mut scene = Scene::new();
        let (ragdoll, _) = build_leg(&mut scene);
        scene
            .try_get_mut(ragdoll)
            .unwrap()
            .ragdoll_mut()
            .unwrap()
            .set_active(true);

        for _ in 0..120 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let hip = scene.world_matrix(root.body).w_axis.truncate();
        let thigh = scene.world_matrix(root.children[0].body).w_axis.truncate();

        // 关节把两节肢体拴在一起，距离该和初始的骨骼间距（0.5）差不多。
        let distance = (hip - thigh).length();
        assert!(distance < 1.0, "肢体散架了，间距 {distance}");
    }

    #[test]
    fn toggling_back_to_inactive_snaps_the_bodies_home_again() {
        let mut scene = Scene::new();
        let (ragdoll, bones) = build_leg(&mut scene);

        scene.try_get_mut(ragdoll).unwrap().ragdoll_mut().unwrap().set_active(true);
        for _ in 0..60 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        // 把骨骼摆回原处，再关掉布娃娃。
        scene.try_get_mut(bones[0]).unwrap().transform =
            Transform::from_position(Vec3::new(0.0, 3.0, 0.0));
        scene.try_get_mut(ragdoll).unwrap().ragdoll_mut().unwrap().set_active(false);

        for _ in 0..5 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let bone_world = scene.world_matrix(root.bone).w_axis.truncate();
        let body_world = scene.world_matrix(root.body).w_axis.truncate();

        assert!(
            (bone_world - body_world).length() < 0.05,
            "关掉后刚体没有回到骨骼上：骨骼 {bone_world:?}，刚体 {body_world:?}"
        );
    }

    #[test]
    fn limbs_are_visited_parents_before_children() {
        // 顺序错了，子骨骼会用上一帧的父骨骼世界变换来换算，动画慢一拍。
        let limb = RagdollLimb {
            bone: Handle::NONE,
            body: Handle::NONE,
            children: vec![RagdollLimb {
                bone: Handle::new(1, 1),
                body: Handle::NONE,
                children: vec![RagdollLimb {
                    bone: Handle::new(2, 1),
                    body: Handle::NONE,
                    children: vec![],
                }],
            }],
        };

        let mut order = Vec::new();
        limb.for_each(&mut |l| order.push(l.bone.index()));

        assert_eq!(order, vec![0, 1, 2]);
    }
}

#[cfg(test)]
mod soldier_test {
    use super::*;
    use crate::{Collider, Scene};
    use kgltf::Model;
    use kphysics::{ColliderDesc, JointKind, RigidBodyDesc, SphericalLimits};

    /// 加载仓库里的 Soldier.glb（Mixamo 骨架，49 关节）。
    fn soldier() -> kasset::Resource<Model> {
        use kasset::{MemoryResourceIo, ResourceManager};
        use std::sync::Arc;

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/Soldier.glb");
        let bytes = std::fs::read(path).expect("仓库里应当有 assets/Soldier.glb");

        let mut io = MemoryResourceIo::new();
        io.add("Soldier.glb", bytes);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(kgltf::GltfLoader);
        manager
            .request_blocking::<Model>("Soldier.glb")
            .expect("Soldier.glb 应当能加载")
    }

    /// 按 Mixamo 的命名搭一套人形布娃娃——demo 里走的就是这条路径。
    fn build_humanoid(scene: &mut Scene) -> Handle<Node> {
        let bone = |scene: &Scene, name: &str| {
            scene
                .find_by_name(&format!("mixamorig:{name}"))
                .unwrap_or_else(|| panic!("找不到骨骼 mixamorig:{name}"))
        };
        let limb = |handle: Handle<Node>, half: f32, radius: f32| {
            LimbDesc::new(handle, ColliderShape::capsule_y(half, radius))
                .with_offset(Vec3::new(0.0, half, 0.0), Quat::IDENTITY)
        };
        let ball = |half_angle: f32| JointDesc {
            kind: JointKind::Spherical {
                limits: SphericalLimits::symmetric(half_angle),
            },
            ..Default::default()
        };
        let hinge = |min: f32, max: f32| JointDesc {
            kind: JointKind::Revolute {
                axis: Vec3::X,
                limits: hinge_limits(min, max),
            },
            ..Default::default()
        };

        let arm = |scene: &Scene, side: &str| {
            limb(bone(scene, &format!("{side}Arm")), 0.13, 0.06)
                .with_joint(ball(1.0))
                .with_child(
                    limb(bone(scene, &format!("{side}ForeArm")), 0.12, 0.05)
                        .with_joint(hinge(-2.2, 0.0)),
                )
        };
        let leg = |scene: &Scene, side: &str| {
            limb(bone(scene, &format!("{side}UpLeg")), 0.2, 0.08)
                .with_joint(ball(0.8))
                .with_child(
                    limb(bone(scene, &format!("{side}Leg")), 0.2, 0.07)
                        .with_joint(hinge(0.0, 2.2)),
                )
        };

        let skeleton = limb(bone(scene, "Hips"), 0.1, 0.12)
            .with_child(
                limb(bone(scene, "Spine1"), 0.12, 0.11)
                    .with_joint(ball(0.4))
                    .with_child(
                        limb(bone(scene, "Spine2"), 0.12, 0.11)
                            .with_joint(ball(0.4))
                            .with_child(limb(bone(scene, "Head"), 0.1, 0.09).with_joint(ball(0.6)))
                            .with_child(arm(scene, "Left"))
                            .with_child(arm(scene, "Right")),
                    ),
            )
            .with_child(leg(scene, "Left"))
            .with_child(leg(scene, "Right"));

        let root = scene.root();
        RagdollBuilder::new(skeleton).build(scene, root)
    }

    /// 站在地面上、装好布娃娃的士兵。
    fn staged_soldier() -> (Scene, Handle<Node>, Handle<Node>) {
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("ground")
                .with_position(Vec3::new(0.0, -0.5, 0.0))
                .with_rigid_body(RigidBody::fixed())
                .with_collider(Collider::new(ColliderDesc::cuboid(Vec3::new(
                    20.0, 0.5, 20.0,
                )))),
        );

        let model = soldier();
        let model = model.data_ref().unwrap();
        let root = scene.root();
        let instance = scene.instantiate_model(&model, root);
        scene.update();

        let ragdoll = build_humanoid(&mut scene);
        (scene, instance, ragdoll)
    }

    #[test]
    fn a_mixamo_skeleton_maps_onto_a_full_humanoid_ragdoll() {
        let (mut scene, _, ragdoll) = staged_soldier();

        let component = scene.try_get(ragdoll).unwrap().ragdoll().unwrap();
        // 髋 + 两节脊椎 + 头 + 两条 2 节的胳膊 + 两条 2 节的腿 = 12
        assert_eq!(component.limb_count(), 12);

        scene.step_physics(1.0 / 60.0);
        // 12 个肢体刚体 + 地面。
        assert_eq!(scene.physics().body_count(), 13);
        // 根肢体没有父，其余 11 节各有一个关节。
        assert_eq!(scene.physics().joint_count(), 11);
    }

    #[test]
    fn limb_bodies_line_up_with_the_real_bones() {
        let (scene, _, ragdoll) = staged_soldier();
        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();

        let mut pairs = Vec::new();
        root.for_each(&mut |limb| pairs.push((limb.bone, limb.body)));

        for (bone, body) in pairs {
            let bone_world = scene.world_matrix(bone).w_axis.truncate();
            let body_world = scene.world_matrix(body).w_axis.truncate();
            assert!(
                (bone_world - body_world).length() < 1e-3,
                "肢体没对齐骨骼：{bone_world:?} vs {body_world:?}"
            );
        }
    }

    #[test]
    fn an_inactive_ragdoll_leaves_the_animated_pose_alone() {
        let (mut scene, _, _) = staged_soldier();
        let hips = scene.find_by_name("mixamorig:Hips").unwrap();
        let before = scene.world_matrix(hips).w_axis.truncate();

        for _ in 0..120 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let after = scene.world_matrix(hips).w_axis.truncate();
        assert!(
            (after - before).length() < 1e-3,
            "未激活的布娃娃动了骨骼：{before:?} → {after:?}"
        );
    }

    #[test]
    fn an_active_ragdoll_makes_the_soldier_collapse_onto_the_ground() {
        let (mut scene, _, ragdoll) = staged_soldier();
        let head = scene.find_by_name("mixamorig:Head").unwrap();
        let head_before = scene.world_matrix(head).w_axis.truncate().y;

        scene
            .try_get_mut(ragdoll)
            .unwrap()
            .ragdoll_mut()
            .unwrap()
            .set_active(true);

        for _ in 0..240 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let head_after = scene.world_matrix(head).w_axis.truncate().y;
        assert!(
            head_after < head_before - 0.5,
            "士兵没倒下：头从 {head_before} 只到 {head_after}"
        );
        // 倒在地上，不是穿过地面掉下去。
        assert!(head_after > -1.0, "士兵穿过了地面：{head_after}");
    }

    #[test]
    fn every_bone_stays_finite_while_the_ragdoll_falls() {
        // 关节的锚点或坐标系算错时，最典型的症状就是求解器发散成 NaN，
        // 画面上表现为角色瞬间消失——这类错误必须在测试里拦住。
        let (mut scene, _, ragdoll) = staged_soldier();
        scene
            .try_get_mut(ragdoll)
            .unwrap()
            .ragdoll_mut()
            .unwrap()
            .set_active(true);

        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let mut bones = Vec::new();
        root.for_each(&mut |limb| bones.push(limb.bone));

        for _ in 0..240 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
            for &bone in &bones {
                let p = scene.world_matrix(bone).w_axis.truncate();
                assert!(p.is_finite(), "骨骼位置发散成了 {p:?}");
                assert!(p.length() < 100.0, "骨骼被甩到了 {p:?}");
            }
        }
    }

    #[test]
    fn limbs_stay_connected_to_their_parents_while_falling() {
        let (mut scene, _, ragdoll) = staged_soldier();
        scene
            .try_get_mut(ragdoll)
            .unwrap()
            .ragdoll_mut()
            .unwrap()
            .set_active(true);

        // 记下初始的父子间距，倒下后不该差太多——关节就是干这个的。
        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let mut links = Vec::new();
        fn collect(limb: &RagdollLimb, out: &mut Vec<(Handle<Node>, Handle<Node>)>) {
            for child in &limb.children {
                out.push((limb.body, child.body));
                collect(child, out);
            }
        }
        collect(&root, &mut links);

        let initial: Vec<f32> = links
            .iter()
            .map(|(a, b)| {
                (scene.world_matrix(*a).w_axis.truncate()
                    - scene.world_matrix(*b).w_axis.truncate())
                .length()
            })
            .collect();

        for _ in 0..240 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        for ((a, b), expected) in links.iter().zip(initial) {
            let now = (scene.world_matrix(*a).w_axis.truncate()
                - scene.world_matrix(*b).w_axis.truncate())
            .length();
            assert!(
                (now - expected).abs() < 0.1,
                "关节被拉开了：{expected} → {now}"
            );
        }
    }

    #[test]
    fn a_ray_can_pick_a_ragdoll_limb() {
        // demo 里的「开火」就是这么工作的：射线打到肢体，回到节点句柄。
        let (mut scene, _, ragdoll) = staged_soldier();
        scene.step_physics(1.0 / 60.0);

        let hips_body = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().body;
        let origin = scene.world_matrix(hips_body).w_axis.truncate() + Vec3::Z * 5.0;

        let hit = scene
            .cast_ray(&kphysics::RayCastOptions::new(origin, Vec3::NEG_Z, 20.0))
            .expect("射线该打到士兵身上");

        assert!(hit.body_node.is_some());
        let name = scene.try_get(hit.body_node.unwrap()).unwrap().name.clone();
        assert!(name.starts_with("Limb:"), "打到的是「{name}」");
    }

    #[test]
    fn a_ragdoll_replays_identically_from_the_same_starting_pose() {
        fn run_once() -> Vec3 {
            let (mut scene, _, ragdoll) = staged_soldier();
            scene
                .try_get_mut(ragdoll)
                .unwrap()
                .ragdoll_mut()
                .unwrap()
                .set_active(true);
            for _ in 0..180 {
                scene.step_physics(1.0 / 60.0);
                scene.update();
            }
            let head = scene.find_by_name("mixamorig:Head").unwrap();
            scene.world_matrix(head).w_axis.truncate()
        }

        assert_eq!(run_once(), run_once());
    }

    #[test]
    fn an_impulse_knocks_the_ragdoll_sideways() {
        let (mut scene, _, ragdoll) = staged_soldier();
        scene
            .try_get_mut(ragdoll)
            .unwrap()
            .ragdoll_mut()
            .unwrap()
            .set_active(true);
        scene.step_physics(1.0 / 60.0);

        let hips_body = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().body;
        scene
            .try_get_mut(hips_body)
            .unwrap()
            .rigid_body_mut()
            .unwrap()
            .apply_impulse(Vec3::X * 30.0);

        for _ in 0..120 {
            scene.step_physics(1.0 / 60.0);
            scene.update();
        }

        let hips = scene.find_by_name("mixamorig:Hips").unwrap();
        let x = scene.world_matrix(hips).w_axis.truncate().x;
        assert!(x > 0.5, "冲量没把布娃娃推开：x = {x}");
    }

    /// 只在需要一份质量参考时才用：布娃娃总质量应当在人体量级。
    #[test]
    fn limb_masses_add_up_to_something_humanlike() {
        let (mut scene, _, ragdoll) = staged_soldier();
        scene.step_physics(1.0 / 60.0);

        let root = scene.try_get(ragdoll).unwrap().ragdoll().unwrap().root().clone();
        let mut bodies = Vec::new();
        root.for_each(&mut |limb| bodies.push(limb.body));

        // 运动学刚体的质量是 0，先切成动态再量。
        for &body in &bodies {
            scene
                .try_get_mut(body)
                .unwrap()
                .rigid_body_mut()
                .unwrap()
                .set_body_type(RigidBodyType::Dynamic);
        }
        scene.step_physics(1.0 / 60.0);

        let total: f32 = bodies
            .iter()
            .filter_map(|&b| scene.try_get(b)?.rigid_body()?.native())
            .filter_map(|n| scene.physics().body(n).map(|b| b.mass()))
            .sum();

        // 默认密度 1、胶囊按实际尺寸算出来的量级；不是真人体重，
        // 但至少不该是 0 或者上千。
        assert!(total > 0.05 && total < 500.0, "布娃娃总质量 {total}");
        let _ = RigidBodyDesc::default();
    }
}
