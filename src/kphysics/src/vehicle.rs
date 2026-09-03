//! 射线载具：一个刚体车身 + 几条往下打的射线当轮子。
//!
//! # 为什么不用真的轮子
//!
//! 把轮子做成刚体、用铰链接到车身上，是物理上「最诚实」的做法，也是
//! **最难调好**的做法：轮胎和地面之间只有一个接触点，稍微开快一点就
//! 会穿进地里；悬挂要靠关节的弹簧模拟，参数一动整台车就跳起来。
//! 商业引擎（以及 Bullet、rapier、PhysX）给的方案都不是它。
//!
//! 通行做法是**射线载具**：车身是一个刚体，每个轮子只是一条从车身往下
//! 打的射线。射线打到地面就按压缩量施加悬挂力，再沿前进方向和侧向各施加
//! 一个摩擦冲量。轮子本身不参与碰撞检测，所以永远不会卡住、不会穿透，
//! 而且只有一个刚体要解算。
//!
//! 代价说清楚：
//!
//! | | 射线载具 | 真轮子 |
//! |---|---|---|
//! | 轮子压过台阶 | 一条射线，抬起来就是抬起来了 | 会真的被棱卡一下 |
//! | 侧翻 | 车身仍会翻，但轮子不会「拖地」 | 更真实 |
//! | 调参 | 悬挂刚度/阻尼几个数，直觉性强 | 关节参数互相耦合 |
//! | 稳定性 | 高速也不穿 | 要开 CCD 还未必够 |
//!
//! # 轮子是画出来的，不是模拟出来的
//!
//! [`VehicleController::wheel_pose`] 给的是渲染用的位姿——包括悬挂压缩
//! 之后的高度和轮子转过的角度。场景里那几个轮子节点是**跟着这个位姿走**
//! 的普通网格，没有刚体也没有碰撞体。给它们加碰撞体反而会和射线打架。

use crate::{Axis, BodyHandle, PhysicsWorld};
use kmath::{Quat, Vec3};
use rapier3d::control::{DynamicRayCastVehicleController, WheelTuning as RapierWheelTuning};

/// 悬挂与轮胎的调校参数。
///
/// 默认值来自 rapier，是一台「偏硬的小车」。改的时候一次只动一个：
/// 这几项互相影响，一起动会分不清是谁的效果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelTuning {
    /// 悬挂刚度。大了硬、小了软。太小车会一直下沉到底。
    pub suspension_stiffness: f32,
    /// 压缩时的阻尼。
    pub suspension_compression: f32,
    /// 回弹时的阻尼。
    ///
    /// 通常要比压缩阻尼大一点：不然车压下去之后会弹回来过头，
    /// 表现为过一个坎之后车身上下晃好几下。
    pub suspension_damping: f32,
    /// 悬挂最多能压缩多少（世界单位）。
    pub max_suspension_travel: f32,
    /// 侧向摩擦刚度。小了会漂移，大了转向像贴着轨道走。
    pub side_friction_stiffness: f32,
    /// 轮胎的抓地系数。
    ///
    /// 这是**唯一**能让车打滑的旋钮：给大了油门到底也不会转圈，
    /// 给小了轻点油门就原地烧胎。
    pub friction_slip: f32,
    /// 悬挂能给出的最大力，防止极端压缩时把车弹飞。
    pub max_suspension_force: f32,
}

impl Default for WheelTuning {
    fn default() -> Self {
        let native = RapierWheelTuning::default();
        Self {
            suspension_stiffness: native.suspension_stiffness,
            suspension_compression: native.suspension_compression,
            suspension_damping: native.suspension_damping,
            max_suspension_travel: native.max_suspension_travel,
            side_friction_stiffness: native.side_friction_stiffness,
            friction_slip: native.friction_slip,
            max_suspension_force: native.max_suspension_force,
        }
    }
}

impl WheelTuning {
    fn to_rapier(self) -> RapierWheelTuning {
        RapierWheelTuning {
            suspension_stiffness: self.suspension_stiffness,
            suspension_compression: self.suspension_compression,
            suspension_damping: self.suspension_damping,
            max_suspension_travel: self.max_suspension_travel,
            side_friction_stiffness: self.side_friction_stiffness,
            friction_slip: self.friction_slip,
            max_suspension_force: self.max_suspension_force,
        }
    }
}

/// 装一个轮子要给的参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelDesc {
    /// 轮子挂在车身上的位置，**车身局部空间**。
    ///
    /// 这是悬挂的上端（车身那一头），不是轮心。轮心在它下方
    /// `suspension_rest_length` 处。
    pub connection: Vec3,
    /// 悬挂伸展的方向，车身局部空间。往下就是 `-Y`。
    pub suspension_direction: Vec3,
    /// 轮轴方向，车身局部空间。
    ///
    /// # 四个轮子必须给**同一个**方向
    ///
    /// 驱动方向是 `地面法线 × 轮轴` 算出来的，所以轮轴反了那个轮子就
    /// 朝反方向推。左右各给 `+X` 和 `-X`（一个很自然的写法）会让车
    /// 一边往前一边往后——结果是斜着螃蟹步走，而且**不报任何错**。
    ///
    /// 这不是笔误式的错误，是「物理上左右轮确实共用一根轴」这件事在
    /// 代码里的样子：真车的左右轮转起来是同向的，只是从车的两侧看
    /// 方向相反。
    ///
    /// 默认 `+X`，配合地面法线 `+Y` 得到驱动方向 `-Z`——正好是引擎里
    /// 节点的前方。
    pub axle: Vec3,
    /// 悬挂完全伸展时的长度。
    pub suspension_rest_length: f32,
    /// 轮子半径。射线从连接点往下打这么长再加悬挂行程。
    pub radius: f32,
    /// 悬挂与轮胎的调校。
    pub tuning: WheelTuning,
}

impl WheelDesc {
    /// 一个常规的轮子：往下悬挂、绕 +X 转。
    ///
    /// `connection` 是车身局部空间的挂点。左右轮的区别只在挂点的 X 分量，
    /// **轮轴四个轮子都一样**（见 [`axle`](Self::axle)）。
    pub fn new(connection: Vec3, radius: f32) -> Self {
        Self {
            connection,
            suspension_direction: Vec3::NEG_Y,
            axle: Vec3::X,
            suspension_rest_length: 0.25,
            radius,
            tuning: WheelTuning::default(),
        }
    }

    /// 换一套调校。
    pub fn with_tuning(mut self, tuning: WheelTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// 换悬挂的自然长度。
    pub fn with_suspension_rest_length(mut self, length: f32) -> Self {
        self.suspension_rest_length = length.max(0.0);
        self
    }
}

/// 一个轮子这一帧的状态，给渲染和判断用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelState {
    /// 轮心的世界坐标。
    pub center: Vec3,
    /// 轮子的世界朝向。
    ///
    /// 把一个**沿 Y 轴**的圆柱（[`kmesh::Mesh::cylinder`] 就是）摆成
    /// 这个轮子该有的样子：轴对齐轮轴，再转过它滚过的角度。
    pub rotation: Quat,
    /// 这个轮子这一帧有没有踩在地上。
    ///
    /// 四个轮子全 `false` 就是腾空了——玩家的输入这时候不该起作用，
    /// 否则空中踩油门车会凭空加速。
    pub grounded: bool,
    /// 悬挂当前的长度。等于自然长度说明完全伸展（悬空或刚接触）。
    pub suspension_length: f32,
    /// 转向角（弧度）。
    pub steering: f32,
}

/// 一台射线载具。
///
/// 车身是一个**普通的动态刚体**（自己建、自己给碰撞体），这个控制器
/// 只负责每帧往下打几条射线、算悬挂力和轮胎摩擦，再把力加到车身上。
///
/// # 用法
///
/// ```ignore
/// let mut car = VehicleController::new(chassis_body);
/// for offset in corners {
///     car.add_wheel(&WheelDesc::new(offset, 0.3));
/// }
/// // 每帧：先给输入，再步进物理，最后读位姿去摆轮子的网格。
/// car.set_engine_force(2, throttle);
/// car.set_steering(0, angle);
/// world.update_vehicle(&mut car, dt);
/// ```
///
/// **顺序要紧**：`update_vehicle` 要在 `step` 之前调。它做的是「往车身上
/// 加这一帧的力」，加完才轮到求解器。反过来的话力会晚一帧生效，
/// 表现为方向盘和油门都有明显的延迟。
pub struct VehicleController {
    inner: DynamicRayCastVehicleController,
    chassis: BodyHandle,
}

impl std::fmt::Debug for VehicleController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VehicleController")
            .field("wheels", &self.inner.wheels().len())
            .field("speed", &self.inner.current_vehicle_speed)
            .finish()
    }
}

impl VehicleController {
    /// 给一个已经存在的刚体装上载具控制器。
    ///
    /// 这个刚体必须是**动态**的，而且得有碰撞体——没有碰撞体就没有质量，
    /// 悬挂力加上去什么都不会发生。
    pub fn new(chassis: BodyHandle) -> Self {
        let mut inner = DynamicRayCastVehicleController::new(chassis.0);
        // rapier 默认车头朝 **X**，而这个引擎里节点的前方是 **-Z**
        // （`Transform::looking_at` 的约定）。不改的话车会「侧着开」——
        // 物理完全正常，只是模型的车头对着侧面，而且没有任何报错。
        inner.index_forward_axis = 2;
        inner.index_up_axis = 1;
        Self { inner, chassis }
    }

    /// 换车头朝哪根轴。
    ///
    /// 默认是 Z（配合 `-Z` 是前方的节点约定）。车身模型的朝向和默认不一样
    /// 时才需要动它——比如从别的引擎导进来的、车头朝 X 的模型。
    pub fn with_forward_axis(mut self, axis: Axis) -> Self {
        self.inner.index_forward_axis = match axis {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        };
        self
    }

    /// 换「上」是哪根轴。默认 Y。
    pub fn with_up_axis(mut self, axis: Axis) -> Self {
        self.inner.index_up_axis = match axis {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        };
        self
    }

    /// 车身的刚体句柄。
    pub fn chassis(&self) -> BodyHandle {
        self.chassis
    }

    /// 装一个轮子，返回它的下标。之后所有操作都用这个下标。
    pub fn add_wheel(&mut self, desc: &WheelDesc) -> usize {
        let index = self.inner.wheels().len();
        self.inner.add_wheel(
            crate::convert::to_rv(desc.connection),
            crate::convert::to_rv(desc.suspension_direction.normalize_or(Vec3::NEG_Y)),
            crate::convert::to_rv(desc.axle.normalize_or(Vec3::X)),
            desc.suspension_rest_length,
            desc.radius,
            &desc.tuning.to_rapier(),
        );
        index
    }

    /// 有几个轮子。
    pub fn wheel_count(&self) -> usize {
        self.inner.wheels().len()
    }

    /// 给某个轮子加驱动力（牛顿）。负值倒车。
    ///
    /// **每帧都要设**：这个值不会自己归零，松开油门时得显式设回 0，
    /// 不然车会一直加速到侧翻。
    pub fn set_engine_force(&mut self, wheel: usize, force: f32) {
        if let Some(w) = self.inner.wheels_mut().get_mut(wheel) {
            w.engine_force = force;
        }
    }

    /// 给某个轮子加刹车力。同样每帧都要设。
    pub fn set_brake(&mut self, wheel: usize, brake: f32) {
        if let Some(w) = self.inner.wheels_mut().get_mut(wheel) {
            w.brake = brake.max(0.0);
        }
    }

    /// 设某个轮子的转向角（弧度）。
    pub fn set_steering(&mut self, wheel: usize, radians: f32) {
        if let Some(w) = self.inner.wheels_mut().get_mut(wheel) {
            w.steering = radians;
        }
    }

    /// 车沿前进方向的速度（米/秒）。倒车时为负。
    ///
    /// 「前进」按引擎的约定是 `-Z`。rapier 内部按 `+Z` 记符号
    /// （它只认轴的下标，认不了正负），所以这里取反。
    /// 那个值在 rapier 里只是个供人读的量，不参与任何解算，取反是安全的。
    pub fn speed(&self) -> f32 {
        -self.inner.current_vehicle_speed
    }

    /// 某个轮子这一帧的状态。下标越界时返回 [`None`]。
    pub fn wheel(&self, index: usize) -> Option<WheelState> {
        let wheel = self.inner.wheels().get(index)?;
        let info = wheel.raycast_info();

        let axle = crate::convert::from_rv(wheel.axle()).normalize_or(Vec3::X);
        // 网格是沿 Y 轴的圆柱，先把 Y 转到轮轴上，再转过它滚了多少。
        //
        // 两步不能反：`from_rotation_arc` 给的是「把 Y 摆到轴上」的最短旋转，
        // 而滚动是绕**已经摆好的**那根轴转。反过来的话轮子会绕一根
        // 还没摆正的轴转，看起来像在画锥。
        let align = Quat::from_rotation_arc(Vec3::Y, axle);
        let spin = Quat::from_axis_angle(axle, wheel.rotation);

        Some(WheelState {
            center: crate::convert::from_rv(wheel.center()),
            rotation: spin * align,
            grounded: info.is_in_contact,
            suspension_length: info.suspension_length,
            steering: wheel.steering,
        })
    }

    /// 有没有任何一个轮子踩在地上。
    ///
    /// 全腾空时玩家的输入不该起作用——空中踩油门车会凭空加速，
    /// 而那是这类载具最常见的穿帮。
    pub fn any_wheel_grounded(&self) -> bool {
        self.inner
            .wheels()
            .iter()
            .any(|wheel| wheel.raycast_info().is_in_contact)
    }
}

impl PhysicsWorld {
    /// 跑一帧载具：打射线、算悬挂与轮胎力，加到车身上。
    ///
    /// **要在 [`step`](PhysicsWorld::step) 之前调**。它只是往车身上加力，
    /// 真正的积分是 `step` 干的；顺序反了力会晚一帧生效，
    /// 方向盘和油门都会有明显的延迟。
    ///
    /// 射线会**排除车身自己**：不排除的话每个轮子第一个打到的就是车身
    /// 底盘，悬挂长度永远是 0，车会直接瘫在地上。
    pub fn update_vehicle(&mut self, vehicle: &mut VehicleController, dt: f32) {
        // 查询结构必须是新的。刚加完刚体就跑载具的话，射线会打不到
        // 那些还没进广相的东西——表现是车从新生成的地面上穿过去。
        self.update_query_structures();

        let filter = rapier3d::prelude::QueryFilter::new().exclude_rigid_body(vehicle.chassis.0);
        let world = &mut self.inner;
        let queries = world.broad_phase.as_query_pipeline_mut(
            world.narrow_phase.query_dispatcher(),
            &mut world.bodies,
            &mut world.colliders,
            filter,
        );
        vehicle.inner.update_vehicle(dt, queries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColliderDesc, RigidBodyDesc};

    /// 一台停在地面上方的四轮车。
    fn car() -> (PhysicsWorld, VehicleController) {
        let mut world = PhysicsWorld::new();

        let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(100.0, 0.5, 100.0)),
                Some(ground),
                0,
            )
            .unwrap();

        let chassis = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(0.0, 1.5, 0.0)),
            1,
        );
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(0.9, 0.3, 1.8)),
                Some(chassis),
                1,
            )
            .unwrap();

        let mut vehicle = VehicleController::new(chassis);
        // 下标 0/1 是前轮（车头朝 -Z，所以前轮的 z 是负的）。
        for (x, z) in [(-0.8, -1.3), (0.8, -1.3), (-0.8, 1.3), (0.8, 1.3)] {
            vehicle.add_wheel(&WheelDesc::new(Vec3::new(x, -0.2, z), 0.35));
        }
        world.update_query_structures();
        (world, vehicle)
    }

    fn drive(world: &mut PhysicsWorld, vehicle: &mut VehicleController, seconds: f32) {
        let dt = 1.0 / 60.0;
        for _ in 0..(seconds / dt) as usize {
            world.update_vehicle(vehicle, dt);
            world.step(dt);
        }
    }

    #[test]
    fn a_parked_car_rests_on_its_suspension() {
        // 悬挂没起作用的话车会一路陷到底盘贴地——而画面上只是
        // 「车矮了一点」，很容易被当成模型问题。
        let (mut world, mut vehicle) = car();
        drive(&mut world, &mut vehicle, 2.0);

        let height = world.body(vehicle.chassis()).unwrap().position().y;
        assert!(
            height > 0.6,
            "车身停在 y = {height}，悬挂多半没撑住（底盘半高 0.3 + 车轮 0.35）"
        );
        assert!(height < 1.4, "车身停在 y = {height}，悬挂根本没压缩");
        assert!(vehicle.any_wheel_grounded(), "四个轮子一个都没踩到地");
    }

    #[test]
    fn the_engine_force_actually_moves_the_car() {
        // 「装了但没生效」在这里的样子：射线载具的力是加在车身上的，
        // 忘了调 `update_vehicle` 或者顺序反了，车就一动不动。
        let (mut world, mut vehicle) = car();
        drive(&mut world, &mut vehicle, 1.0);
        let start = world.body(vehicle.chassis()).unwrap().position();

        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 400.0);
        }
        drive(&mut world, &mut vehicle, 2.0);

        let end = world.body(vehicle.chassis()).unwrap().position();
        let travelled = (end - start).length();
        assert!(travelled > 1.0, "踩了两秒油门只走了 {travelled} 米");
        assert!(vehicle.speed().abs() > 0.5, "车在动，速度却是 {}", vehicle.speed());
    }

    #[test]
    fn braking_brings_it_back_to_a_stop() {
        // 刹车不生效的话车会一直滑，而摩擦力本身也会让它慢下来——
        // 所以要和「什么都不做」对照，不能只看「最后停没停」。
        let (mut world, mut vehicle) = car();
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 500.0);
        }
        drive(&mut world, &mut vehicle, 2.0);
        let moving = vehicle.speed().abs();
        assert!(moving > 1.0, "先得让它跑起来，实际速度 {moving}");

        // 松油门 + 刹车。
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 0.0);
            vehicle.set_brake(wheel, 60.0);
        }
        drive(&mut world, &mut vehicle, 1.5);
        let braked = vehicle.speed().abs();

        // 对照：同样跑起来之后只松油门、不刹车。
        let (mut world, mut vehicle) = car();
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 500.0);
        }
        drive(&mut world, &mut vehicle, 2.0);
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 0.0);
        }
        drive(&mut world, &mut vehicle, 1.5);
        let coasting = vehicle.speed().abs();

        assert!(
            braked < coasting * 0.5,
            "刹车之后 {braked}，只松油门 {coasting} —— 刹车没起作用"
        );
    }

    #[test]
    fn the_car_drives_along_its_forward_axis() {
        // 车头朝哪边是这个控制器最容易配错的一项：rapier 默认 X，
        // 而引擎的节点前方是 -Z。配错了车会**侧着开**，物理完全正常，
        // 只有模型看起来在螃蟹步——没有任何报错。
        let (mut world, mut vehicle) = car();
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 400.0);
        }
        drive(&mut world, &mut vehicle, 3.0);

        let position = world.body(vehicle.chassis()).unwrap().position();
        // 引擎的前方是 -Z，所以正油门该让 z 变负。
        assert!(
            position.z < -2.0,
            "开了三秒 z = {}，该往 -Z 走两米以上", position.z
        );
        assert!(
            position.x.abs() < position.z.abs() * 0.3,
            "车往侧面(x = {})跑得比往前(z = {})还多 —— 四个轮子的轮轴多半不一致",
            position.x, position.z
        );
        assert!(vehicle.speed() > 0.5, "往前开，速度却是 {}", vehicle.speed());
    }

    #[test]
    fn flipping_one_wheels_axle_makes_the_car_crab_sideways() {
        // 这条是上面那条的反证，也是这个 API 最容易踩的坑：
        // 「左右轮给相反的轮轴」看起来天经地义，实际会让一边往前一边往后。
        let (mut world, mut vehicle) = {
            let (mut world, _) = car();
            // 重建一台，两侧轮轴给反。
            let chassis = world.add_body(
                &RigidBodyDesc::dynamic().with_position(Vec3::new(20.0, 1.5, 0.0)),
                2,
            );
            world
                .add_collider(
                    &ColliderDesc::cuboid(Vec3::new(0.9, 0.3, 1.8)),
                    Some(chassis),
                    2,
                )
                .unwrap();
            let mut vehicle = VehicleController::new(chassis);
            for (x, z) in [(-0.8, -1.3), (0.8, -1.3), (-0.8, 1.3), (0.8, 1.3)] {
                let mut desc = WheelDesc::new(Vec3::new(x, -0.2, z), 0.35);
                // 「很自然」的写法：左右相反。
                desc.axle = if x < 0.0 { Vec3::NEG_X } else { Vec3::X };
                vehicle.add_wheel(&desc);
            }
            world.update_query_structures();
            (world, vehicle)
        };

        let start = world.body(vehicle.chassis()).unwrap().position();
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 400.0);
        }
        drive(&mut world, &mut vehicle, 3.0);
        let end = world.body(vehicle.chassis()).unwrap().position();

        let forward = (start.z - end.z).abs();
        let sideways = (start.x - end.x).abs();
        assert!(
            sideways > forward,
            "轮轴给反了却还是直着开（前 {forward}，侧 {sideways}）——              这条反证没意义了，说明驱动方向不是靠轮轴定的"
        );
    }

    #[test]
    fn steering_turns_the_car() {
        // 转向角不生效的话车只会直着开。和「不打方向」对照，
        // 因为直行本身也会有一点点侧偏。
        let straight = {
            let (mut world, mut vehicle) = car();
            for wheel in 0..vehicle.wheel_count() {
                vehicle.set_engine_force(wheel, 400.0);
            }
            drive(&mut world, &mut vehicle, 3.0);
            world.body(vehicle.chassis()).unwrap().position().x.abs()
        };

        let (mut world, mut vehicle) = car();
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 400.0);
        }
        // 前两个轮子打方向。
        vehicle.set_steering(0, 0.4);
        vehicle.set_steering(1, 0.4);
        drive(&mut world, &mut vehicle, 3.0);
        let turned = world.body(vehicle.chassis()).unwrap().position().x.abs();

        assert!(
            turned > straight + 0.5,
            "打了方向偏了 {turned} 米，不打方向偏了 {straight} 米 —— 转向没生效"
        );
    }

    #[test]
    fn a_wheel_in_the_air_reports_that_it_is_not_grounded() {
        // 全腾空时玩家的输入不该起作用。分不出「踩地」和「腾空」的话，
        // 空中踩油门车会凭空加速——这是射线载具最常见的穿帮。
        let mut world = PhysicsWorld::new();
        let chassis = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec3::new(0.0, 50.0, 0.0)),
            1,
        );
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(0.9, 0.3, 1.8)),
                Some(chassis),
                1,
            )
            .unwrap();
        let mut vehicle = VehicleController::new(chassis);
        vehicle.add_wheel(&WheelDesc::new(Vec3::new(0.0, -0.2, 0.0), 0.35));

        world.update_query_structures();
        world.update_vehicle(&mut vehicle, 1.0 / 60.0);

        assert!(!vehicle.any_wheel_grounded(), "空中的轮子报告说踩到地了");
        assert!(!vehicle.wheel(0).unwrap().grounded);
    }

    #[test]
    fn the_wheel_pose_follows_the_chassis() {
        // 轮子的网格是跟着这个位姿走的。位姿不对的话轮子会浮在车外面，
        // 而物理仍然完全正常——纯粹是个画错了的问题，没有任何报错。
        let (mut world, mut vehicle) = car();
        drive(&mut world, &mut vehicle, 1.0);

        let chassis = world.body(vehicle.chassis()).unwrap().position();
        for index in 0..vehicle.wheel_count() {
            let wheel = vehicle.wheel(index).expect("轮子该存在");
            let offset = wheel.center - chassis;
            assert!(
                offset.length() < 3.0,
                "第 {index} 个轮子离车身 {} 米，位姿多半算错了",
                offset.length()
            );
            assert!(wheel.center.is_finite(), "第 {index} 个轮子的位置是 NaN");
            assert!(wheel.rotation.is_finite(), "第 {index} 个轮子的朝向是 NaN");
            // 轮子在车身下方。
            assert!(offset.y < 0.0, "第 {index} 个轮子跑到车身上面去了");
        }
    }

    #[test]
    fn the_wheels_spin_while_driving() {
        // 滚动角不动的话，轮子会像雪橇一样平移过去。
        let (mut world, mut vehicle) = car();
        for wheel in 0..vehicle.wheel_count() {
            vehicle.set_engine_force(wheel, 400.0);
        }
        drive(&mut world, &mut vehicle, 0.5);
        let before = vehicle.wheel(0).unwrap().rotation;
        drive(&mut world, &mut vehicle, 1.0);
        let after = vehicle.wheel(0).unwrap().rotation;

        assert!(
            before.angle_between(after) > 0.2,
            "开了一秒轮子几乎没转"
        );
    }
}
