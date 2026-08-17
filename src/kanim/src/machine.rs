//! 状态机与混合树。
//!
//! 两者都**不产出姿态**，只回答一个问题：这一帧每个剪辑该占多少权重。
//! 混合那一步交给 [`Animator`](crate::Animator)，全局只有一份实现。
//! 这样状态机可以在没有任何动画数据的情况下测试——权重是纯粹的数值逻辑。
//!
//! 结构取自 Fyrox 的 `machine` 模块：状态里放混合树，状态之间用带时长的过渡连接。

use std::collections::HashMap;

/// 驱动状态机的一个参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Parameter {
    /// 开关量，用于「是否着地」这类条件。
    Bool(bool),
    /// 连续量，用于「速度」这类既做条件又做混合坐标的值。
    Float(f32),
}

impl Parameter {
    /// 取布尔值；类型不符时返回 [`None`]。
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            Self::Float(_) => None,
        }
    }

    /// 取浮点值；布尔会被折算成 0 或 1，这样同一个参数既能做条件又能做混合坐标。
    pub fn as_float(self) -> Option<f32> {
        match self {
            Self::Float(value) => Some(value),
            Self::Bool(value) => Some(if value { 1.0 } else { 0.0 }),
        }
    }
}

/// 参数表。
#[derive(Debug, Clone, Default)]
pub struct Parameters {
    values: HashMap<String, Parameter>,
}

impl Parameters {
    /// 空参数表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置一个参数。
    pub fn set(&mut self, name: impl Into<String>, value: Parameter) {
        self.values.insert(name.into(), value);
    }

    /// 设置一个浮点参数。
    pub fn set_float(&mut self, name: impl Into<String>, value: f32) {
        self.set(name, Parameter::Float(value));
    }

    /// 设置一个布尔参数。
    pub fn set_bool(&mut self, name: impl Into<String>, value: bool) {
        self.set(name, Parameter::Bool(value));
    }

    /// 读取一个参数。
    pub fn get(&self, name: &str) -> Option<Parameter> {
        self.values.get(name).copied()
    }

    /// 读取浮点参数，不存在时给 0。
    pub fn float(&self, name: &str) -> f32 {
        self.get(name).and_then(Parameter::as_float).unwrap_or(0.0)
    }

    /// 读取布尔参数，不存在时给 `false`。
    pub fn bool(&self, name: &str) -> bool {
        self.get(name).and_then(Parameter::as_bool).unwrap_or(false)
    }
}

/// 浮点比较方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// 小于。
    Less,
    /// 小于等于。
    LessOrEqual,
    /// 大于。
    Greater,
    /// 大于等于。
    GreaterOrEqual,
}

impl Compare {
    fn test(self, left: f32, right: f32) -> bool {
        match self {
            Self::Less => left < right,
            Self::LessOrEqual => left <= right,
            Self::Greater => left > right,
            Self::GreaterOrEqual => left >= right,
        }
    }
}

/// 转移条件。
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    /// 恒真，用于无条件转移。
    Always,
    /// 布尔参数等于期望值。
    Bool {
        /// 参数名。
        parameter: String,
        /// 期望值。
        expected: bool,
    },
    /// 浮点参数与阈值的比较。
    Float {
        /// 参数名。
        parameter: String,
        /// 比较方式。
        compare: Compare,
        /// 阈值。
        value: f32,
    },
    /// 两个条件都成立。
    And(Box<Condition>, Box<Condition>),
    /// 两个条件至少一个成立。
    Or(Box<Condition>, Box<Condition>),
    /// 取反。
    Not(Box<Condition>),
}

impl Condition {
    /// 便捷构造：浮点参数大于阈值。
    pub fn greater(parameter: impl Into<String>, value: f32) -> Self {
        Self::Float {
            parameter: parameter.into(),
            compare: Compare::Greater,
            value,
        }
    }

    /// 便捷构造：浮点参数小于阈值。
    pub fn less(parameter: impl Into<String>, value: f32) -> Self {
        Self::Float {
            parameter: parameter.into(),
            compare: Compare::Less,
            value,
        }
    }

    /// 便捷构造：布尔参数为真。
    pub fn is_true(parameter: impl Into<String>) -> Self {
        Self::Bool {
            parameter: parameter.into(),
            expected: true,
        }
    }

    /// 与另一个条件取合取。
    pub fn and(self, other: Self) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }

    /// 与另一个条件取析取。
    pub fn or(self, other: Self) -> Self {
        Self::Or(Box::new(self), Box::new(other))
    }

    /// 取反。
    pub fn negate(self) -> Self {
        Self::Not(Box::new(self))
    }

    /// 在给定参数下求值。参数不存在时按默认值（0 / false）处理。
    pub fn evaluate(&self, parameters: &Parameters) -> bool {
        match self {
            Self::Always => true,
            Self::Bool {
                parameter,
                expected,
            } => parameters.bool(parameter) == *expected,
            Self::Float {
                parameter,
                compare,
                value,
            } => compare.test(parameters.float(parameter), *value),
            Self::And(a, b) => a.evaluate(parameters) && b.evaluate(parameters),
            Self::Or(a, b) => a.evaluate(parameters) || b.evaluate(parameters),
            Self::Not(inner) => !inner.evaluate(parameters),
        }
    }
}

/// 混合树：一个状态内部怎么把若干剪辑组合起来。
#[derive(Debug, Clone, PartialEq)]
pub enum BlendTree {
    /// 直接播一个剪辑。
    Clip(usize),
    /// 按固定权重混合若干子树。
    Blend(Vec<(BlendTree, f32)>),
    /// 一维混合空间：按参数值在相邻两个采样点之间插值。
    ///
    /// 「速度 → 走/跑」这类需求就是它：参数落在两点之间时两边各占一部分。
    BlendSpace1D {
        /// 作为横坐标的参数名。
        parameter: String,
        /// 采样点，按坐标排序（构造时会排好）。
        points: Vec<(f32, BlendTree)>,
    },
}

impl BlendTree {
    /// 构造一维混合空间，自动按坐标排序。
    pub fn blend_space_1d(
        parameter: impl Into<String>,
        points: impl IntoIterator<Item = (f32, BlendTree)>,
    ) -> Self {
        let mut points: Vec<(f32, BlendTree)> = points.into_iter().collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self::BlendSpace1D {
            parameter: parameter.into(),
            points,
        }
    }

    /// 把本树在给定参数下的剪辑权重累加到 `out`。
    ///
    /// `weight` 是外层传下来的权重，逐层相乘——于是嵌套多深都不会破坏归一化。
    pub fn accumulate(&self, parameters: &Parameters, weight: f32, out: &mut Vec<(usize, f32)>) {
        if weight <= 0.0 {
            return;
        }

        match self {
            Self::Clip(clip) => add_weight(out, *clip, weight),
            Self::Blend(children) => {
                let total: f32 = children.iter().map(|(_, w)| w.max(0.0)).sum();
                if total <= 0.0 {
                    return;
                }
                for (child, child_weight) in children {
                    child.accumulate(parameters, weight * child_weight.max(0.0) / total, out);
                }
            }
            Self::BlendSpace1D { parameter, points } => {
                if points.is_empty() {
                    return;
                }
                let coordinate = parameters.float(parameter);

                // 落在两端之外时取端点，不做外插——外插会让权重变成负数。
                if coordinate <= points[0].0 {
                    points[0].1.accumulate(parameters, weight, out);
                    return;
                }
                if coordinate >= points[points.len() - 1].0 {
                    points[points.len() - 1]
                        .1
                        .accumulate(parameters, weight, out);
                    return;
                }

                let upper = points.partition_point(|(position, _)| *position <= coordinate);
                let (left_position, left) = &points[upper - 1];
                let (right_position, right) = &points[upper];
                let span = right_position - left_position;
                let t = if span > f32::EPSILON {
                    (coordinate - left_position) / span
                } else {
                    0.0
                };

                left.accumulate(parameters, weight * (1.0 - t), out);
                right.accumulate(parameters, weight * t, out);
            }
        }
    }

    /// 求出本树在给定参数下的剪辑权重。
    pub fn weights(&self, parameters: &Parameters) -> Vec<(usize, f32)> {
        let mut out = Vec::new();
        self.accumulate(parameters, 1.0, &mut out);
        out
    }
}

/// 同一个剪辑可能被多个分支引用，权重要合并而不是各记一笔。
fn add_weight(out: &mut Vec<(usize, f32)>, clip: usize, weight: f32) {
    if let Some(entry) = out.iter_mut().find(|(existing, _)| *existing == clip) {
        entry.1 += weight;
    } else {
        out.push((clip, weight));
    }
}

/// 状态机里的一个状态。
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// 状态名，用于查找与调试。
    pub name: String,
    /// 该状态播什么。
    pub tree: BlendTree,
}

impl State {
    /// 一个直接播单个剪辑的状态。
    pub fn clip(name: impl Into<String>, clip: usize) -> Self {
        Self {
            name: name.into(),
            tree: BlendTree::Clip(clip),
        }
    }

    /// 一个内部有混合树的状态。
    pub fn new(name: impl Into<String>, tree: BlendTree) -> Self {
        Self {
            name: name.into(),
            tree,
        }
    }
}

/// 两个状态之间的转移。
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// 起始状态序号。
    pub from: usize,
    /// 目标状态序号。
    pub to: usize,
    /// 过渡时长（秒）。为 0 表示瞬切。
    pub duration: f32,
    /// 触发条件。
    pub condition: Condition,
}

impl Transition {
    /// 新建一条转移。
    pub fn new(from: usize, to: usize, duration: f32, condition: Condition) -> Self {
        Self {
            from,
            to,
            duration,
            condition,
        }
    }
}

/// 正在进行的过渡。
#[derive(Debug, Clone, Copy, PartialEq)]
struct ActiveTransition {
    to: usize,
    elapsed: f32,
    duration: f32,
}

/// 动画状态机。
#[derive(Debug, Clone, Default)]
pub struct StateMachine {
    states: Vec<State>,
    transitions: Vec<Transition>,
    current: usize,
    active: Option<ActiveTransition>,
}

impl StateMachine {
    /// 空状态机。
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加一个状态，返回其序号。
    pub fn add_state(&mut self, state: State) -> usize {
        self.states.push(state);
        self.states.len() - 1
    }

    /// 添加一条转移。
    pub fn add_transition(&mut self, transition: Transition) {
        self.transitions.push(transition);
    }

    /// 全部状态。
    pub fn states(&self) -> &[State] {
        &self.states
    }

    /// 按名字找状态序号。
    pub fn state_index(&self, name: &str) -> Option<usize> {
        self.states.iter().position(|state| state.name == name)
    }

    /// 当前状态序号。过渡中返回的是**起点**状态。
    pub fn current(&self) -> usize {
        self.current
    }

    /// 过渡的目标状态；不在过渡中时返回 [`None`]。
    pub fn transitioning_to(&self) -> Option<usize> {
        self.active.map(|active| active.to)
    }

    /// 是否正在过渡。
    pub fn is_transitioning(&self) -> bool {
        self.active.is_some()
    }

    /// 直接跳到某个状态，不走过渡。
    pub fn set_current(&mut self, state: usize) {
        if state < self.states.len() {
            self.current = state;
            self.active = None;
        }
    }

    /// 推进一帧。
    ///
    /// 过渡进行中不再接受新的转移请求——否则条件在阈值附近抖动时，
    /// 状态机会在两个状态间反复横跳，动画看起来就像卡住了。
    pub fn update(&mut self, dt: f32, parameters: &Parameters) {
        if self.states.is_empty() {
            return;
        }

        if let Some(active) = &mut self.active {
            active.elapsed += dt;
            if active.elapsed >= active.duration {
                self.current = active.to;
                self.active = None;
            }
            return;
        }

        let Some(transition) = self
            .transitions
            .iter()
            .find(|t| t.from == self.current && t.to != self.current && t.condition.evaluate(parameters))
        else {
            return;
        };
        if transition.to >= self.states.len() {
            return;
        }

        if transition.duration <= 0.0 {
            self.current = transition.to;
        } else {
            self.active = Some(ActiveTransition {
                to: transition.to,
                elapsed: 0.0,
                duration: transition.duration,
            });
        }
    }

    /// 求出这一帧各剪辑的权重。
    ///
    /// 过渡期间两个状态的权重按过渡进度此消彼长，加起来恒为 1。
    pub fn weights(&self, parameters: &Parameters) -> Vec<(usize, f32)> {
        let mut out = Vec::new();
        let Some(current) = self.states.get(self.current) else {
            return out;
        };

        match self.active {
            Some(active) => {
                let t = if active.duration > 0.0 {
                    (active.elapsed / active.duration).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                current.tree.accumulate(parameters, 1.0 - t, &mut out);
                if let Some(target) = self.states.get(active.to) {
                    target.tree.accumulate(parameters, t, &mut out);
                }
            }
            None => current.tree.accumulate(parameters, 1.0, &mut out),
        }

        out
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn parameters(speed: f32) -> Parameters {
        let mut parameters = Parameters::new();
        parameters.set_float("speed", speed);
        parameters
    }

    /// Idle ⇄ Walk，过渡各 0.2 秒。
    fn walk_machine() -> StateMachine {
        let mut machine = StateMachine::new();
        let idle = machine.add_state(State::clip("Idle", 0));
        let walk = machine.add_state(State::clip("Walk", 1));
        machine.add_transition(Transition::new(
            idle,
            walk,
            0.2,
            Condition::greater("speed", 0.1),
        ));
        machine.add_transition(Transition::new(
            walk,
            idle,
            0.2,
            Condition::less("speed", 0.1),
        ));
        machine
    }

    fn weight_of(weights: &[(usize, f32)], clip: usize) -> f32 {
        weights
            .iter()
            .find(|(index, _)| *index == clip)
            .map(|(_, weight)| *weight)
            .unwrap_or(0.0)
    }

    #[test]
    fn starts_in_the_first_state() {
        let machine = walk_machine();

        assert_eq!(machine.current(), 0);
        assert_eq!(weight_of(&machine.weights(&parameters(0.0)), 0), 1.0);
    }

    #[test]
    fn condition_starts_a_transition() {
        let mut machine = walk_machine();

        machine.update(0.0, &parameters(1.0));

        assert!(machine.is_transitioning());
        assert_eq!(machine.transitioning_to(), Some(1));
        // 刚开始过渡，权重还全在起点状态上。
        let weights = machine.weights(&parameters(1.0));
        assert!((weight_of(&weights, 0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn transition_weights_cross_fade_and_sum_to_one() {
        let mut machine = walk_machine();
        machine.update(0.0, &parameters(1.0));
        machine.update(0.1, &parameters(1.0));

        let weights = machine.weights(&parameters(1.0));
        let (idle, walk) = (weight_of(&weights, 0), weight_of(&weights, 1));

        assert!((idle - 0.5).abs() < 1e-5, "起点权重 {idle}");
        assert!((walk - 0.5).abs() < 1e-5, "终点权重 {walk}");
        assert!((idle + walk - 1.0).abs() < 1e-5);
    }

    #[test]
    fn transition_completes_and_settles() {
        let mut machine = walk_machine();
        machine.update(0.0, &parameters(1.0));
        machine.update(0.25, &parameters(1.0));

        assert!(!machine.is_transitioning());
        assert_eq!(machine.current(), 1);
        assert_eq!(weight_of(&machine.weights(&parameters(1.0)), 1), 1.0);
    }

    #[test]
    fn transitions_are_not_interrupted() {
        let mut machine = walk_machine();
        machine.update(0.0, &parameters(1.0));

        // 过渡刚开始就把条件反转：不能立刻掉头，否则参数在阈值附近抖动时
        // 状态机会反复横跳，动画看起来像卡住了。
        machine.update(0.05, &parameters(0.0));

        assert_eq!(machine.transitioning_to(), Some(1));
    }

    #[test]
    fn zero_duration_transition_is_instant() {
        let mut machine = StateMachine::new();
        let a = machine.add_state(State::clip("A", 0));
        let b = machine.add_state(State::clip("B", 1));
        machine.add_transition(Transition::new(a, b, 0.0, Condition::Always));

        machine.update(0.0, &Parameters::new());

        assert!(!machine.is_transitioning());
        assert_eq!(machine.current(), b);
    }

    #[test]
    fn self_transitions_are_ignored() {
        let mut machine = StateMachine::new();
        let a = machine.add_state(State::clip("A", 0));
        machine.add_transition(Transition::new(a, a, 0.5, Condition::Always));

        machine.update(0.1, &Parameters::new());

        assert!(!machine.is_transitioning());
    }

    #[test]
    fn empty_machine_produces_no_weights() {
        let mut machine = StateMachine::new();

        machine.update(1.0, &Parameters::new());

        assert!(machine.weights(&Parameters::new()).is_empty());
    }

    #[test]
    fn blend_space_interpolates_between_points() {
        let tree = BlendTree::blend_space_1d(
            "speed",
            [
                (0.0, BlendTree::Clip(0)),
                (1.0, BlendTree::Clip(1)),
                (5.0, BlendTree::Clip(2)),
            ],
        );

        // 恰好落在采样点上：全给那一个。
        let at_point = tree.weights(&parameters(1.0));
        assert!((weight_of(&at_point, 1) - 1.0).abs() < 1e-6);

        // 落在 1 与 5 之间：两边按距离分。
        let between = tree.weights(&parameters(3.0));
        assert!((weight_of(&between, 1) - 0.5).abs() < 1e-6);
        assert!((weight_of(&between, 2) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn blend_space_clamps_outside_its_range() {
        let tree = BlendTree::blend_space_1d(
            "speed",
            [(0.0, BlendTree::Clip(0)), (1.0, BlendTree::Clip(1))],
        );

        // 外插会让权重变成负数，所以两端一律夹紧。
        assert!((weight_of(&tree.weights(&parameters(-10.0)), 0) - 1.0).abs() < 1e-6);
        assert!((weight_of(&tree.weights(&parameters(10.0)), 1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn blend_space_sorts_its_points() {
        // 乱序传入也要能正确插值。
        let tree = BlendTree::blend_space_1d(
            "speed",
            [(5.0, BlendTree::Clip(2)), (0.0, BlendTree::Clip(0))],
        );

        let weights = tree.weights(&parameters(2.5));
        assert!((weight_of(&weights, 0) - 0.5).abs() < 1e-6);
        assert!((weight_of(&weights, 2) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn nested_trees_stay_normalised() {
        // 混合树套混合空间：无论嵌套多深，总权重都应当是 1。
        let tree = BlendTree::Blend(vec![
            (BlendTree::Clip(0), 1.0),
            (
                BlendTree::blend_space_1d(
                    "speed",
                    [(0.0, BlendTree::Clip(1)), (1.0, BlendTree::Clip(2))],
                ),
                3.0,
            ),
        ]);

        let weights = tree.weights(&parameters(0.5));
        let total: f32 = weights.iter().map(|(_, weight)| weight).sum();

        assert!((total - 1.0).abs() < 1e-6, "总权重 {total}");
        assert!((weight_of(&weights, 0) - 0.25).abs() < 1e-6);
        assert!((weight_of(&weights, 1) - 0.375).abs() < 1e-6);
        assert!((weight_of(&weights, 2) - 0.375).abs() < 1e-6);
    }

    #[test]
    fn repeated_clips_have_their_weights_merged() {
        // 同一个剪辑出现在多个分支里，权重要合并而不是各记一笔。
        let tree = BlendTree::Blend(vec![(BlendTree::Clip(0), 1.0), (BlendTree::Clip(0), 1.0)]);

        let weights = tree.weights(&Parameters::new());

        assert_eq!(weights.len(), 1);
        assert!((weight_of(&weights, 0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_blend_produces_nothing() {
        assert!(BlendTree::Blend(Vec::new()).weights(&Parameters::new()).is_empty());
        assert!(
            BlendTree::blend_space_1d("speed", [])
                .weights(&Parameters::new())
                .is_empty()
        );
    }

    #[test]
    fn conditions_compose() {
        let mut parameters = Parameters::new();
        parameters.set_float("speed", 5.0);
        parameters.set_bool("grounded", true);

        let condition = Condition::greater("speed", 1.0).and(Condition::is_true("grounded"));
        assert!(condition.evaluate(&parameters));

        let negated = condition.clone().negate();
        assert!(!negated.evaluate(&parameters));

        parameters.set_bool("grounded", false);
        assert!(!condition.evaluate(&parameters));
        assert!(
            Condition::greater("speed", 1.0)
                .or(Condition::is_true("grounded"))
                .evaluate(&parameters)
        );
    }

    #[test]
    fn missing_parameters_fall_back_to_defaults() {
        let parameters = Parameters::new();

        // 参数没设置时按 0 / false 处理，而不是 panic。
        assert!(!Condition::is_true("nope").evaluate(&parameters));
        assert!(Condition::less("nope", 1.0).evaluate(&parameters));
        assert_eq!(parameters.float("nope"), 0.0);
    }

    #[test]
    fn bool_parameters_read_as_floats() {
        let mut parameters = Parameters::new();
        parameters.set_bool("grounded", true);

        // 同一个参数既能做条件又能做混合坐标。
        assert_eq!(parameters.float("grounded"), 1.0);
    }
}
