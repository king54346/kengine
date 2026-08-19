//! 物理调试渲染。
//!
//! rapier 自带一整套调试渲染流水线，它把碰撞体、关节、接触点全部拆成线段，
//! 对外只要求一件事：**能画一条带颜色的线**。所以这里的工作量几乎为零——
//! 实现一个转发到回调的 backend，再把 rapier 的开关翻译成本地的类型即可。
//!
//! # 为什么是回调而不是直接画
//!
//! kphysics 是 rapier 的唯一出口，它不该反过来知道「调试线画到哪里去」。
//! 回调让调用方（kscene）自己决定：塞进 gizmo 缓冲、打日志、还是丢掉。
//!
//! # 颜色是 HSLA
//!
//! rapier 给的颜色是 `[色相 0..=360, 饱和度, 亮度, 不透明度]`，不是 RGBA。
//! 这里**原样透传**，不在这一层转换——kphysics 没有颜色类型，凭空造一个
//! 只会和上层的那个打架。调用方拿到后用 `Color::from_hsla` 转。

use crate::PhysicsWorld;
use kmath::Vec3;
use rapier3d::pipeline::{
    DebugRenderBackend, DebugRenderMode, DebugRenderObject, DebugRenderPipeline,
};

/// 画哪些东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicsDebugOptions {
    /// 碰撞体的线框。最常用的一项。
    pub collider_shapes: bool,
    /// 碰撞体的包围盒——看宽阶段在拿什么做判断。
    pub collider_aabbs: bool,
    /// 刚体的局部坐标轴。
    pub body_axes: bool,
    /// 关节的锚点与连线。
    pub joints: bool,
    /// 几何接触点与接触法线。
    ///
    /// 单独列出来是因为它**特别吵**：一摞箱子就能画出上百个接触点。
    /// 排查「为什么这两个东西没碰上」的时候才打开。
    pub contacts: bool,
}

impl Default for PhysicsDebugOptions {
    /// 默认只画碰撞体线框、刚体坐标轴和关节——接触点太吵，默认关掉。
    fn default() -> Self {
        Self {
            collider_shapes: true,
            collider_aabbs: false,
            body_axes: true,
            joints: true,
            contacts: false,
        }
    }
}

impl PhysicsDebugOptions {
    /// 只画碰撞体线框。
    pub fn shapes_only() -> Self {
        Self {
            collider_shapes: true,
            collider_aabbs: false,
            body_axes: false,
            joints: false,
            contacts: false,
        }
    }

    /// 什么都画，包括很吵的接触点。
    pub fn all() -> Self {
        Self {
            collider_shapes: true,
            collider_aabbs: true,
            body_axes: true,
            joints: true,
            contacts: true,
        }
    }

    /// 什么都不画。
    pub fn none() -> Self {
        Self {
            collider_shapes: false,
            collider_aabbs: false,
            body_axes: false,
            joints: false,
            contacts: false,
        }
    }

    /// 一项都没开。
    pub fn is_empty(&self) -> bool {
        !(self.collider_shapes
            || self.collider_aabbs
            || self.body_axes
            || self.joints
            || self.contacts)
    }

    fn to_mode(self) -> DebugRenderMode {
        let mut mode = DebugRenderMode::empty();
        mode.set(DebugRenderMode::COLLIDER_SHAPES, self.collider_shapes);
        mode.set(DebugRenderMode::COLLIDER_AABBS, self.collider_aabbs);
        mode.set(DebugRenderMode::RIGID_BODY_AXES, self.body_axes);
        mode.set(DebugRenderMode::JOINTS, self.joints);
        mode.set(DebugRenderMode::CONTACTS, self.contacts);
        mode
    }
}

/// 把 rapier 的画线请求转发给一个闭包。
struct CallbackBackend<'a> {
    draw: &'a mut dyn FnMut(Vec3, Vec3, [f32; 4]),
}

impl DebugRenderBackend for CallbackBackend<'_> {
    fn draw_line(
        &mut self,
        _object: DebugRenderObject<'_>,
        a: rapier3d::math::Vector,
        b: rapier3d::math::Vector,
        color: [f32; 4],
    ) {
        (self.draw)(Vec3::new(a.x, a.y, a.z), Vec3::new(b.x, b.y, b.z), color);
    }
}

impl PhysicsWorld {
    /// 把物理世界拆成线段交给 `draw`。
    ///
    /// 每条线给出两个世界坐标端点和一个 **HSLA** 颜色
    /// （`[色相 0..=360, 饱和度, 亮度, 不透明度]`）。
    ///
    /// 每次调用都会重新建一条 rapier 的调试流水线。它内部只缓存形状的
    /// 网格化结果，重建的代价是一次分配——比起为了它在 `PhysicsWorld` 里
    /// 常驻一份「只有开了调试才用得上」的状态，这样更干净。
    pub fn debug_render(
        &self,
        options: PhysicsDebugOptions,
        draw: &mut dyn FnMut(Vec3, Vec3, [f32; 4]),
    ) {
        if options.is_empty() {
            return;
        }

        let mut pipeline = DebugRenderPipeline::new(Default::default(), options.to_mode());
        let mut backend = CallbackBackend { draw };
        pipeline.render(
            &mut backend,
            &self.inner.bodies,
            &self.inner.colliders,
            &self.inner.impulse_joints,
            &self.inner.multibody_joints,
            &self.inner.narrow_phase,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColliderDesc, RigidBodyDesc};
    use kmath::Vec3;

    /// 收集调试线，返回 (线段数, 是否全是有限值)。
    fn collect(world: &PhysicsWorld, options: PhysicsDebugOptions) -> (usize, bool) {
        let mut count = 0;
        let mut finite = true;
        world.debug_render(options, &mut |a, b, _| {
            count += 1;
            finite &= a.is_finite() && b.is_finite();
        });
        (count, finite)
    }

    fn world_with_a_box() -> PhysicsWorld {
        let mut world = PhysicsWorld::new();
        let body = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec3::Y), 0);
        world.add_collider(&ColliderDesc::cuboid(Vec3::splat(0.5)), Some(body), 0);
        world
    }

    #[test]
    fn an_empty_world_draws_nothing() {
        let world = PhysicsWorld::new();
        let (count, _) = collect(&world, PhysicsDebugOptions::all());
        assert_eq!(count, 0);
    }

    #[test]
    fn a_collider_produces_lines() {
        let world = world_with_a_box();
        let (count, finite) = collect(&world, PhysicsDebugOptions::shapes_only());
        assert!(count > 0, "碰撞体应当画出线框");
        assert!(finite, "不该出现 NaN 或无穷大的端点");
    }

    #[test]
    fn turning_everything_off_short_circuits() {
        let world = world_with_a_box();
        let (count, _) = collect(&world, PhysicsDebugOptions::none());
        assert_eq!(count, 0);
    }

    #[test]
    fn body_axes_add_lines_on_top_of_shapes() {
        let world = world_with_a_box();
        let (shapes, _) = collect(&world, PhysicsDebugOptions::shapes_only());

        let mut with_axes = PhysicsDebugOptions::shapes_only();
        with_axes.body_axes = true;
        let (total, _) = collect(&world, with_axes);

        // 三根轴。
        assert_eq!(total, shapes + 3);
    }

    #[test]
    fn aabbs_are_off_unless_asked_for() {
        let world = world_with_a_box();
        let (without, _) = collect(&world, PhysicsDebugOptions::shapes_only());

        let mut with_aabbs = PhysicsDebugOptions::shapes_only();
        with_aabbs.collider_aabbs = true;
        let (with, _) = collect(&world, with_aabbs);

        // 包围盒是十二条棱。
        assert_eq!(with, without + 12);
    }

    #[test]
    fn the_shape_of_the_collider_changes_the_line_count() {
        // 球体要靠许多段逼近，盒子只有十二条棱——数量差得出来，
        // 说明画的确实是形状本身，不是某种占位物。
        let mut world = PhysicsWorld::new();
        world.add_collider(&ColliderDesc::cuboid(Vec3::splat(0.5)), None, 0);
        let (cuboid_lines, _) = collect(&world, PhysicsDebugOptions::shapes_only());

        let mut world = PhysicsWorld::new();
        world.add_collider(&ColliderDesc::ball(0.5), None, 0);
        let (ball_lines, _) = collect(&world, PhysicsDebugOptions::shapes_only());

        assert!(
            ball_lines > cuboid_lines,
            "球 {ball_lines} 段，盒子 {cuboid_lines} 段"
        );
    }

    #[test]
    fn colors_come_back_as_hsla_not_rgba() {
        // 色相超出 [0,1]，说明这确实是 HSLA——当成 RGBA 用会得到一片惨白。
        let world = world_with_a_box();
        let mut max_first = 0.0f32;
        world.debug_render(PhysicsDebugOptions::shapes_only(), &mut |_, _, c| {
            max_first = max_first.max(c[0]);
        });
        assert!(
            max_first > 1.0,
            "第一分量是色相（0..=360），实测 {max_first}"
        );
    }

    #[test]
    fn contacts_show_up_only_after_the_bodies_touch() {
        let mut world = PhysicsWorld::new();
        world.add_collider(&ColliderDesc::cuboid(Vec3::new(5.0, 0.5, 5.0)), None, 0);
        let body = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec3::Y * 5.0), 1);
        world.add_collider(&ColliderDesc::ball(0.5), Some(body), 1);

        let mut options = PhysicsDebugOptions::none();
        options.contacts = true;

        // 掉下来之前隔得远，没有接触。
        world.step(1.0 / 60.0);
        let (before, _) = collect(&world, options);
        assert_eq!(before, 0);

        // 落到地面上。
        for _ in 0..180 {
            world.step(1.0 / 60.0);
        }
        let (after, _) = collect(&world, options);
        assert!(after > 0, "落地后应当有接触点");
    }
}
