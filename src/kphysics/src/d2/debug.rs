//! 2D 物理的调试渲染。
//!
//! 和 3D 那份（[`crate::debug`]）是同一套做法：rapier 自带调试流水线，把
//! 碰撞体、关节、接触点拆成线段，对外只要求「能画一条带颜色的线」。
//! 这里实现一个转发到回调的 backend，再把开关翻译一下。
//!
//! 线段端点是 [`Vec2`]——2D 世界里没有第三维，往哪个平面上画由调用方决定
//! （引擎里通常是 XY 平面，z = 0）。
//!
//! 颜色同样是 **HSLA**（`[色相 0..=360, 饱和度, 亮度, 不透明度]`），
//! 原样透传，理由见 3D 那份的说明。

use super::PhysicsWorld;
use kmath::Vec2;
use rapier2d::pipeline::{
    DebugRenderBackend, DebugRenderMode, DebugRenderObject, DebugRenderPipeline,
};

/// 画哪些东西。
///
/// 和 3D 的 [`PhysicsDebugOptions`](crate::PhysicsDebugOptions) 字段一致，
/// 但**是两个类型**——2D 与 3D 是两个独立的世界，共用一个选项类型会让人
/// 以为它们能互相传。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicsDebugOptions2d {
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

impl Default for PhysicsDebugOptions2d {
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

impl PhysicsDebugOptions2d {
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

    /// 只画碰撞体线框。
    pub fn shapes_only() -> Self {
        Self {
            collider_shapes: true,
            ..Self::none()
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
    draw: &'a mut dyn FnMut(Vec2, Vec2, [f32; 4]),
}

impl DebugRenderBackend for CallbackBackend<'_> {
    fn draw_line(
        &mut self,
        _object: DebugRenderObject<'_>,
        a: rapier2d::math::Vector,
        b: rapier2d::math::Vector,
        color: [f32; 4],
    ) {
        (self.draw)(Vec2::new(a.x, a.y), Vec2::new(b.x, b.y), color);
    }
}

impl PhysicsWorld {
    /// 把物理世界拆成线段交给 `draw`。
    ///
    /// 每条线给出两个世界坐标端点和一个 **HSLA** 颜色。
    ///
    /// 每次调用都会重新建一条 rapier 的调试流水线，理由同 3D 那份：
    /// 它内部只缓存形状的网格化结果，重建就是一次分配，比为它在
    /// `PhysicsWorld` 里常驻一份「只有开了调试才用得上」的状态更干净。
    pub fn debug_render(
        &self,
        options: PhysicsDebugOptions2d,
        draw: &mut dyn FnMut(Vec2, Vec2, [f32; 4]),
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
    use crate::d2::{ColliderDesc, JointDesc, RigidBodyDesc};

    /// 收集调试线，返回 (线段数, 是否全是有限值)。
    fn collect(world: &PhysicsWorld, options: PhysicsDebugOptions2d) -> (usize, bool) {
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
        let body = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec2::Y), 0);
        world.add_collider(&ColliderDesc::cuboid(Vec2::splat(0.5)), Some(body), 0);
        world
    }

    #[test]
    fn shapes_produce_lines() {
        let world = world_with_a_box();
        let (count, finite) = collect(&world, PhysicsDebugOptions2d::shapes_only());

        // 一个矩形至少四条边。
        assert!(count >= 4, "只画出了 {count} 条线");
        assert!(finite, "画出了 NaN 或无穷的端点");
    }

    #[test]
    fn an_empty_option_set_draws_nothing() {
        let world = world_with_a_box();
        assert_eq!(collect(&world, PhysicsDebugOptions2d::none()).0, 0);
    }

    #[test]
    fn turning_on_axes_adds_lines() {
        let world = world_with_a_box();

        let shapes = collect(&world, PhysicsDebugOptions2d::shapes_only()).0;
        let with_axes = collect(
            &world,
            PhysicsDebugOptions2d {
                body_axes: true,
                ..PhysicsDebugOptions2d::shapes_only()
            },
        )
        .0;

        assert!(with_axes > shapes, "开了坐标轴反而没多画");
    }

    #[test]
    fn joints_are_drawn() {
        let mut world = PhysicsWorld::new();
        let anchor = world.add_body(&RigidBodyDesc::fixed(), 0);
        let swinging = world.add_body(
            &RigidBodyDesc::dynamic().with_position(Vec2::new(1.0, 0.0)),
            1,
        );
        world.add_joint(
            anchor,
            swinging,
            &JointDesc::revolute(Vec2::ZERO, Vec2::new(-1.0, 0.0), None),
        );

        let options = PhysicsDebugOptions2d {
            joints: true,
            ..PhysicsDebugOptions2d::none()
        };
        let (count, finite) = collect(&world, options);

        assert!(count > 0, "关节一条线都没画");
        assert!(finite);
    }

    #[test]
    fn an_empty_world_draws_nothing() {
        let world = PhysicsWorld::new();
        assert_eq!(collect(&world, PhysicsDebugOptions2d::default()).0, 0);
    }
}
