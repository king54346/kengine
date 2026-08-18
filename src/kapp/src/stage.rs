//! 更新阶段。
//!
//! 一帧内的执行顺序是**固定**的——不是 ECS 那种按依赖关系自动排序的
//! System 图，而是一条写死的流水线。场景图架构下这样更简单也更可预测。

/// 一帧内的各个阶段，按执行先后排列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// 采集输入。引擎在此之前已把窗口事件喂给 `Input`。
    Input,
    /// 游戏逻辑主体。绝大多数代码写在这里。
    Update,
    /// 逻辑收尾。适合放依赖 `Update` 结果的处理，如相机跟随。
    PostUpdate,
    /// 推进物理并与场景图同步。引擎内置。
    ///
    /// 挂在这里的系统跑在物理步进**之后**，正好读碰撞事件与新的位姿。
    Physics,
    /// 重算世界变换与包围盒。引擎内置，用户一般不往这里挂。
    Transform,
    /// 可见性剔除。
    Culling,
    /// 提交绘制。
    Render,
    /// 帧末清理，如重置输入的「刚按下」标记。
    FrameEnd,
}

impl Stage {
    /// 按执行顺序排列的全部阶段。
    pub const ORDER: [Stage; 8] = [
        Stage::Input,
        Stage::Update,
        Stage::PostUpdate,
        Stage::Physics,
        Stage::Transform,
        Stage::Culling,
        Stage::Render,
        Stage::FrameEnd,
    ];

    /// 用户逻辑通常可以安全挂载的阶段。
    ///
    /// `Physics` / `Transform` / `Culling` / `Render` 由引擎内置流程占用，
    /// 往里挂东西需要清楚自己在做什么。
    pub fn is_user_stage(&self) -> bool {
        matches!(
            self,
            Stage::Input | Stage::Update | Stage::PostUpdate | Stage::FrameEnd
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn order_lists_every_stage_once() {
        let mut seen = Stage::ORDER.to_vec();
        seen.sort();
        seen.dedup();

        assert_eq!(seen.len(), Stage::ORDER.len(), "ORDER 中有重复阶段");
    }

    #[test]
    fn order_matches_declaration_order() {
        // 枚举的声明顺序即执行顺序，两者不一致会让调度出现意外。
        let mut sorted = Stage::ORDER;
        sorted.sort();

        assert_eq!(sorted, Stage::ORDER);
    }

    #[test]
    fn physics_runs_between_logic_and_transform() {
        // 物理写的是局部变换，必须排在世界变换重算之前；
        // 又要读逻辑这一帧施加的力，所以得排在 PostUpdate 之后。
        assert!(Stage::PostUpdate < Stage::Physics);
        assert!(Stage::Physics < Stage::Transform);
    }

    #[test]
    fn transform_runs_after_update() {
        // 逻辑改完变换后才重算世界矩阵，顺序颠倒会导致画面延迟一帧。
        assert!(Stage::Update < Stage::Transform);
        assert!(Stage::Transform < Stage::Culling);
        assert!(Stage::Culling < Stage::Render);
    }

    #[test]
    fn engine_owned_stages_are_not_user_stages() {
        assert!(Stage::Update.is_user_stage());
        assert!(!Stage::Transform.is_user_stage());
        assert!(!Stage::Render.is_user_stage());
    }
}
