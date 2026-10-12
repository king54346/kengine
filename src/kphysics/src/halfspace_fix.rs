// 由 `halfspace_fix` 模块（3D）和 `d2::halfspace_fix`（2D）各 `include!` 一次，
// 调用方先把 `rapier` 指到 rapier3d / rapier2d。

use rapier::geometry::{ContactData, ContactManifoldData};
use rapier::math::{Pose, Real, Vector};
use rapier::parry::query::details::{ShapeCastOptions, contact_halfspace_support_map};
use rapier::parry::query::{
    ClosestPoints, Contact, ContactManifold, ContactManifoldsWorkspace, DefaultQueryDispatcher,
    NonlinearRigidMotion, PersistentQueryDispatcher, QueryDispatcher, QueryDispatcherChain,
    ShapeCastHit, Unsupported,
};
use rapier::parry::shape::{Shape, ShapeType};

/// 修 parry 0.30 的一个 bug：`contact(形状, 半空间)`（半空间在第二个）时，
/// `contact_support_map_halfspace` 把 `pos12` 原样交给了要逆变换的
/// `contact_halfspace_support_map`（同一文件里 distance / closest_points / cast_shapes
/// 的对应函数都有 `.inverse()`，唯独它漏了）。
///
/// 后果：任何离半空间多远的形状都被判成「深深陷在里面」。角色控制器每次移动前的
/// 解穿透就是这么问的，于是场景里只要有一块 `HalfSpace`，角色就每帧被往上推
/// （0.9 米高的胶囊每帧 0.2 米多），哪怕半空间在脚下几十米。
///
/// 这里只接管「第二个是半空间、第一个不是」的 `contact`，其余一律 `Unsupported`，
/// 交回默认的 dispatcher。parry 修好之后整个模块可以删掉。
pub(crate) struct HalfSpaceContactFix;

impl QueryDispatcher for HalfSpaceContactFix {
    fn intersection_test(
        &self,
        _: &Pose,
        _: &dyn Shape,
        _: &dyn Shape,
    ) -> Result<bool, Unsupported> {
        Err(Unsupported)
    }

    fn distance(&self, _: &Pose, _: &dyn Shape, _: &dyn Shape) -> Result<Real, Unsupported> {
        Err(Unsupported)
    }

    fn contact(
        &self,
        pos12: &Pose,
        g1: &dyn Shape,
        g2: &dyn Shape,
        prediction: Real,
    ) -> Result<Option<Contact>, Unsupported> {
        if g1.shape_type() == ShapeType::HalfSpace {
            return Err(Unsupported);
        }
        let (Some(halfspace), Some(other)) = (g2.as_halfspace(), g1.as_support_map()) else {
            return Err(Unsupported);
        };
        Ok(
            contact_halfspace_support_map(&pos12.inverse(), halfspace, other, prediction)
                .map(|contact| contact.flipped()),
        )
    }

    fn closest_points(
        &self,
        _: &Pose,
        _: &dyn Shape,
        _: &dyn Shape,
        _: Real,
    ) -> Result<ClosestPoints, Unsupported> {
        Err(Unsupported)
    }

    fn cast_shapes(
        &self,
        _: &Pose,
        _: Vector,
        _: &dyn Shape,
        _: &dyn Shape,
        _: ShapeCastOptions,
    ) -> Result<Option<ShapeCastHit>, Unsupported> {
        Err(Unsupported)
    }

    fn cast_shapes_nonlinear(
        &self,
        _: &NonlinearRigidMotion,
        _: &dyn Shape,
        _: &NonlinearRigidMotion,
        _: &dyn Shape,
        _: Real,
        _: Real,
        _: bool,
    ) -> Result<Option<ShapeCastHit>, Unsupported> {
        Err(Unsupported)
    }
}

impl PersistentQueryDispatcher<ContactManifoldData, ContactData> for HalfSpaceContactFix {
    // 接触流形（仿真用的那条路）没有这个 bug，全交给默认实现。
    fn contact_manifolds(
        &self,
        _: &Pose,
        _: &dyn Shape,
        _: &dyn Shape,
        _: Real,
        _: &mut Vec<ContactManifold<ContactManifoldData, ContactData>>,
        _: &mut Option<ContactManifoldsWorkspace>,
    ) -> Result<(), Unsupported> {
        Err(Unsupported)
    }

    fn contact_manifold_convex_convex(
        &self,
        _: &Pose,
        _: &dyn Shape,
        _: &dyn Shape,
        _: Option<&dyn rapier::parry::query::details::NormalConstraints>,
        _: Option<&dyn rapier::parry::query::details::NormalConstraints>,
        _: Real,
        _: &mut ContactManifold<ContactManifoldData, ContactData>,
    ) -> Result<(), Unsupported> {
        Err(Unsupported)
    }
}

/// 带修正的窄相：先问 [`HalfSpaceContactFix`]，不管的再交给默认 dispatcher。
pub(crate) fn narrow_phase() -> rapier::geometry::NarrowPhase {
    let dispatcher: QueryDispatcherChain<HalfSpaceContactFix, DefaultQueryDispatcher> =
        HalfSpaceContactFix.chain(DefaultQueryDispatcher);
    rapier::geometry::NarrowPhase::with_query_dispatcher(dispatcher)
}
