//! 浮力：在船体上放若干探针，每个探针按自己泡进水里多深产生向上的力。
//!
//! 力加在探针的位置上，所以一侧泡得深的时候会产生力矩——船随浪摇晃、
//! 浪头打到船头时船头被抬起来，都是这么来的。
//!
//! 两种模式：
//!
//! - **多点**（[`Buoyancy::hull`]）：船体上铺一片探针，各自按泡进水多深出力——船有完整的俯仰和横摇。
//! - **单点**（[`Buoyancy::point`]）：只在中心查一次海面，上下浮动，再用一个扶正力矩把物体轻轻转到
//!   和水面法线对齐。浮标、木箱、漂浮的碎片用它就够了，省得每个小东西查一堆点。
//!
//! 阻尼按探针**相对于水**的速度算：浪里的水在绕圈（浪峰处往前、浪谷处往后），
//! 漂浮物被它带着走。按绝对速度算的话阻尼会把船钉在原地、浪头直接漫过去。

use super::Ocean;
use kcore::pool::Handle;
use kmath::{Mat4, Vec3};
use kscene::{Node, Scene};

/// 一个浮力探针。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuoyancyProbe {
    /// 在物体局部空间的位置。
    pub local: Vec3,
    /// 完全淹没时提供的浮力（牛顿）。
    pub max_force: f32,
}

/// 状态：一组探针、阻尼，以及上一步每个探针的深度（调试显示用）。
#[derive(Debug, Clone)]
pub struct Buoyancy {
    pub probes: Vec<BuoyancyProbe>,
    /// 每根水柱的高度（米），以探针为中点。整根泡进水里算完全淹没。
    pub probe_height: f32,
    /// 竖直方向的阻尼（N·s/m，每个探针）。
    pub vertical_damping: f32,
    /// 水平方向的阻尼（水的阻力）。
    pub horizontal_damping: f32,
    /// 上一步每个探针的世界位置和淹没比例（0–1）。
    pub last: Vec<(Vec3, f32)>,
    /// 上一步每个探针受的力（牛顿），调试显示用。
    pub forces: Vec<Vec3>,
    /// 单点模式的扶正力矩系数（N·m/弧度）；0 = 多点模式（力矩来自各探针的力）。
    pub upright: f32,
}

impl Buoyancy {
    /// 在一个长方体（船体的包围盒）底面铺 `nx × nz` 个探针，总浮力 `total_force`。
    ///
    /// 总浮力是船重的 f 倍时平衡在吃水 1/f 处：2 倍吃水一半，3 倍吃水三分之一。
    pub fn hull(half_extents: Vec3, nx: usize, nz: usize, total_force: f32) -> Self {
        let mut probes = Vec::new();
        let count = (nx * nz).max(1) as f32;
        for iz in 0..nz {
            for ix in 0..nx {
                let fx = if nx > 1 {
                    ix as f32 / (nx - 1) as f32 * 2.0 - 1.0
                } else {
                    0.0
                };
                let fz = if nz > 1 {
                    iz as f32 / (nz - 1) as f32 * 2.0 - 1.0
                } else {
                    0.0
                };
                // 船头船尾收窄一点：探针沿船身的位置越靠两头，横向越往里收。
                let taper = 1.0 - 0.45 * fz.abs().powi(2);
                // 探针放在船体中间高度，各管一根贯穿船体上下的水柱。放在船底的话浮力全从
                // 质心下面往上顶，是个倒立摆——轻的东西（浮标）会翻过来漂着。
                probes.push(BuoyancyProbe {
                    local: Vec3::new(
                        fx * half_extents.x * 0.85 * taper,
                        0.0,
                        fz * half_extents.z * 0.9,
                    ),
                    max_force: total_force / count,
                });
            }
        }
        Self {
            probes,
            probe_height: half_extents.y * 2.0,
            vertical_damping: total_force / count * 0.25,
            horizontal_damping: total_force / count * 0.05,
            last: Vec::new(),
            forces: Vec::new(),
            upright: 0.0,
        }
    }

    /// 单点浮力：一根以物体中心为中点、高 `height` 米的水柱，总浮力 `total_force`。
    ///
    /// 和 [`Buoyancy::hull`] 一样，总浮力是重量的 f 倍时平衡在吃水 1/f 处。
    /// 不靠探针之间的力差产生力矩，而是直接把物体往水面法线方向扶：随浪轻轻摇摆，不会翻。
    pub fn point(height: f32, total_force: f32) -> Self {
        Self {
            probes: vec![BuoyancyProbe {
                local: Vec3::ZERO,
                max_force: total_force,
            }],
            probe_height: height,
            vertical_damping: total_force * 0.25,
            horizontal_damping: total_force * 0.05,
            last: Vec::new(),
            forces: Vec::new(),
            upright: total_force * height * 0.15,
        }
    }

    /// 算出浮力并加到 `body` 节点的刚体上。在定长步（`fixed_update`）里每步调一次。
    pub fn apply(&mut self, scene: &mut Scene, body: Handle<Node>, ocean: &Ocean) {
        let Some(node) = scene.try_get(body) else {
            return;
        };
        let Some(rigid) = node.rigid_body() else {
            return;
        };
        let transform: Mat4 = node.global_transform();
        let center = transform.w_axis.truncate();
        let (linvel, angvel) = (rigid.linvel(), rigid.angvel());

        let mut force = Vec3::ZERO;
        let mut torque = Vec3::ZERO;
        self.last.clear();
        self.forces.clear();
        for probe in &self.probes {
            let world = transform.transform_point3(probe.local);
            let water = ocean.sample(world.x, world.z);
            // 水柱从探针往下半个船高到往上半个船高：泡进去多少比例就给多少浮力。
            // 总浮力是重量的 f 倍时，平衡在吃水 1/f 处（2 倍 = 吃水一半）。
            let bottom = world.y - self.probe_height * 0.5;
            let submerged = ((water.height - bottom) / self.probe_height).clamp(0.0, 1.0);
            self.last.push((world, submerged));
            if submerged <= 0.0 {
                self.forces.push(Vec3::ZERO);
                continue;
            }
            // 探针的速度 = 质心速度 + ω × r；阻尼按它相对于水的速度。
            let r = world - center;
            let relative = linvel + angvel.cross(r) - water.velocity;
            // 浮力沿水面法线而不是正上方：浪坡上的东西会被推下坡（冲浪就是这么回事）。
            let up = Vec3::new(water.normal.x * 0.25, 1.0, water.normal.z * 0.25).normalize();
            let mut f = up * probe.max_force * submerged;
            f.y -= relative.y * self.vertical_damping * submerged;
            f.x -= relative.x * self.horizontal_damping * submerged;
            f.z -= relative.z * self.horizontal_damping * submerged;
            force += f;
            torque += r.cross(f);
            self.forces.push(f);
        }
        if self.upright > 0.0 {
            // 单点：力都在质心上、没有力矩。扶正：把物体的上方向转向水面法线，再给转动加阻尼。
            let submerged = self.last.first().map_or(0.0, |l| l.1);
            let water = ocean.sample(center.x, center.z);
            let up = transform.y_axis.truncate().normalize_or(Vec3::Y);
            torque =
                (up.cross(water.normal) * self.upright - angvel * self.upright * 0.3) * submerged;
        }
        if let Some(rigid) = scene.try_get_mut(body).and_then(Node::rigid_body_mut) {
            rigid.add_force(force);
            rigid.add_torque(torque);
        }
    }

    /// 淹没的探针占比（0–1）。
    pub fn submerged_fraction(&self) -> f32 {
        if self.last.is_empty() {
            return 0.0;
        }
        self.last.iter().map(|(_, s)| *s).sum::<f32>() / self.last.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocean::{Ocean, OceanSettings};
    use crate::quality::Quality;
    use kphysics::{ColliderDesc, RigidBodyDesc};
    use kscene::{Collider, RigidBody};

    /// 返回每秒一次的 (浮标高度, 浮标处的水面高度)。
    fn simulate(half: Vec3, mass: f32, factor: f32, steps: usize) -> Vec<(f32, f32)> {
        let mut scene = Scene::new();
        let mut ocean = Ocean::new(OceanSettings::default(), Quality::Low);
        ocean.spawn(&mut scene);
        let volume = half.x * half.y * half.z * 8.0;
        let body = scene.add_node(
            Node::new("float")
                .with_position(Vec3::new(10.0, 0.3, 5.0))
                .with_rigid_body(RigidBody::new(
                    RigidBodyDesc::dynamic().with_damping(0.05, 1.0),
                ))
                .with_collider(Collider::new(
                    ColliderDesc::cuboid(half).with_density(mass / volume),
                )),
        );
        let mut buoyancy = Buoyancy::hull(half, 2, 2, mass * 9.81 * factor);
        let dt = 1.0 / 60.0;
        let mut samples = Vec::new();
        for step in 0..steps {
            ocean.update(&mut scene, Vec3::ZERO, dt);
            scene.update();
            buoyancy.apply(&mut scene, body, &ocean);
            scene.step_physics(dt);
            scene.update();
            if step % 30 == 0 && step > 120 {
                let p = scene[body].global_position();
                samples.push((p.y, ocean.height_at(p.x, p.z)));
            }
        }
        samples
    }

    #[test]
    fn a_point_float_bobs_upright() {
        let mut scene = Scene::new();
        let mut ocean = Ocean::new(OceanSettings::default(), Quality::Low);
        ocean.spawn(&mut scene);
        let half = Vec3::new(0.4, 0.4, 0.4);
        let mass = 60.0;
        let body = scene.add_node(
            Node::new("crate")
                .with_position(Vec3::new(-6.0, 1.0, 3.0))
                .with_rotation(kmath::Quat::from_rotation_z(0.6))
                .with_rigid_body(RigidBody::new(
                    RigidBodyDesc::dynamic().with_damping(0.05, 0.5),
                ))
                .with_collider(Collider::new(
                    ColliderDesc::cuboid(half)
                        .with_density(mass / (half.x * half.y * half.z * 8.0)),
                )),
        );
        let mut buoyancy = Buoyancy::point(half.y * 2.0, mass * 9.81 * 2.5);
        let dt = 1.0 / 60.0;
        let mut offsets = Vec::new();
        let mut tilt: f32 = 0.0;
        for step in 0..900 {
            ocean.update(&mut scene, Vec3::ZERO, dt);
            scene.update();
            buoyancy.apply(&mut scene, body, &ocean);
            scene.step_physics(dt);
            scene.update();
            if step > 300 {
                let node = &scene[body];
                let p = node.global_position();
                offsets.push(p.y - ocean.height_at(p.x, p.z));
                tilt = tilt.max(
                    node.global_transform()
                        .y_axis
                        .truncate()
                        .normalize()
                        .angle_between(Vec3::Y),
                );
            }
        }
        let mean = offsets.iter().sum::<f32>() / offsets.len() as f32;
        assert!(mean > -0.4 && mean < 0.5, "单点浮体的平均吃水不对：{mean}");
        assert!(tilt < 0.6, "单点浮体应该被扶正，最大倾角 {tilt} 弧度");
    }

    #[test]
    fn a_small_buoy_rides_the_waves() {
        let samples = simulate(Vec3::new(0.45, 0.5, 0.45), 120.0, 3.0, 900);
        // 跟着浪走：浮标高度减去它那里的水面高度，应该一直在一个小范围里。
        let offsets: Vec<f32> = samples.iter().map(|(y, water)| y - water).collect();
        let mean = offsets.iter().sum::<f32>() / offsets.len() as f32;
        let worst = offsets.iter().map(|o| (o - mean).abs()).fold(0.0, f32::max);
        let swing = samples.iter().map(|s| s.1).fold(f32::MIN, f32::max)
            - samples.iter().map(|s| s.1).fold(f32::MAX, f32::min);
        // 3 倍浮力 → 吃水三分之一：质心比水面高 1/6 个高度（约 0.17 米）。
        assert!(
            mean > -0.2 && mean < 0.5,
            "浮标的平均吃水不对：{mean}（{offsets:?}）"
        );
        assert!(
            worst < swing * 0.5 + 0.3,
            "浮标没跟上浪：偏离 {worst}，水面起伏 {swing}"
        );
    }
}
