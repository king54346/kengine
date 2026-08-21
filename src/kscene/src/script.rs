//! 脚本槽位：节点上「挂了哪个脚本」的标记。
//!
//! **kscene 不认识脚本引擎。** 这里只存一个路径字符串和几个状态位，
//! 真正的 VM、生命周期、引擎 API 全在 `kscript` 里——它反过来依赖 kscene，
//! 因为脚本要实时读写场景。
//!
//! 这么分的两个好处：
//! - 分层不破：boa 只有 kscript 认识，与「wgpu 只有 krender 认识」同一个道理；
//! - **脚本能随场景存档**：槽位里只有字符串，序列化天然可行。

/// 挂在节点上的脚本槽位。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptSlot {
    /// 脚本资源路径，例如 `scripts/spin.js`。
    pub path: String,
    /// 是否参与执行。
    pub enabled: bool,
    /// 运行时里的实例编号；[`ScriptSlot::NO_INSTANCE`] 表示还没实例化。
    ///
    /// 存成裸 `u32` 而不是 kscript 的类型：这个模块不该认识脚本引擎。
    pub instance: u32,
    /// 实例化失败过就不再重试——源码有语法错误的话，重试一万次也一样。
    pub failed: bool,
    /// 脚本自己存下来的状态，一段 JSON。
    ///
    /// 脚本的闭包变量（`let hp = 100;` 那种）**存不下来**——它们活在
    /// JS 的闭包里，Rust 这边够不着，而且里面可能有函数、有循环引用。
    /// 所以是**显式**的：脚本实现 `_save()` 返回一个可 JSON 化的对象，
    /// 实现 `_load(state)` 把它读回去。两个都不实现就没有状态。
    ///
    /// 存档时由 `ScriptRuntime::save_states` 填，读档后由
    /// `ScriptRuntime` 在实例化时喂回给脚本。
    pub state: String,
}

impl ScriptSlot {
    /// 「还没实例化」的哨兵值。
    pub const NO_INSTANCE: u32 = u32::MAX;

    /// 挂上指定路径的脚本。
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            enabled: true,
            instance: Self::NO_INSTANCE,
            failed: false,
            state: String::new(),
        }
    }

    /// 是否已经实例化。
    pub fn is_live(&self) -> bool {
        self.instance != Self::NO_INSTANCE
    }

    /// 换一份脚本。旧实例会在下一次 tick 时被销毁重建。
    pub fn set_path(&mut self, path: impl Into<String>) {
        self.path = path.into();
        self.instance = Self::NO_INSTANCE;
        self.failed = false;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Node, Scene};

    #[test]
    fn a_fresh_slot_is_not_live_yet() {
        let slot = ScriptSlot::new("a.js");

        assert!(!slot.is_live());
        assert!(slot.enabled);
        assert!(!slot.failed);
    }

    #[test]
    fn changing_the_path_forces_a_rebuild() {
        let mut slot = ScriptSlot::new("a.js");
        slot.instance = 3;
        slot.failed = true;

        slot.set_path("b.js");

        assert_eq!(slot.path, "b.js");
        assert!(!slot.is_live(), "换了脚本旧实例该作废");
        assert!(!slot.failed, "换了脚本该重新给一次机会");
    }

    #[test]
    fn a_script_node_shows_up_in_the_scene_index() {
        // kscript 每帧靠这个索引找到该跑的脚本，不必扫整个节点池。
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("s").with_script("a.js"));
        scene.update();

        assert_eq!(scene.script_nodes(), &[node]);
    }
}
