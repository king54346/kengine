//! 滑条。值由调用方保管，拖动、方向键、轨道点击在这里算。
//!
//! # 为什么值是 `&mut f32`
//!
//! 别的控件都只报告「被点了」，让调用方自己改状态。滑条不行：从一次
//! 拖动的**像素增量**换算回值，要知道轨道多长、滑块多粗、值域多宽、
//! 精度是几位——这些只有控件自己知道。让调用方算的结果是每个调用点
//! 都抄一遍同一段换算，抄错一处就是那个滑条拖起来跟手速不一致。
//!
//! 所以这里和 [`text_input`](crate::WidgetUi::text_input) 一样破例直接改
//! 传进来的值。**状态仍然在调用方**——控件不保存它，只是替你算。

use std::ops::RangeInclusive;

use kmath::{Vec2, Vec4};
use kui::{Id, NavKey, Rect, Response, Ui, UiInput};

use crate::widgets::{Declared, Theme, Widget, WidgetUi};

/// 滑条的朝向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// 看形状定：又高又窄的是竖的，其余是横的。
    #[default]
    Auto,
    /// 强制横着。
    Horizontal,
    /// 强制竖着。
    Vertical,
}

impl Orientation {
    /// 这个矩形上，这条滑条是不是竖的。
    pub fn is_vertical(self, rect: Rect) -> bool {
        match self {
            Orientation::Auto => rect.size().y > rect.size().x,
            Orientation::Horizontal => false,
            Orientation::Vertical => true,
        }
    }
}

/// 点在**轨道**上（而不是滑块上）会怎样。
///
/// 只管按下的那一刻；按住之后的拖动三种都一样跟手。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackClick {
    /// 不改值，就当按在滑块上，接着拖。
    #[default]
    Drag,
    /// 朝点击的方向加减一步，像点滚动条的轨道那样翻页。
    Step,
    /// 直接跳到点的位置。
    Snap,
}

/// 滑条的值域、步进、精度与朝向。
///
/// ```
/// use kui_widgets::Slider;
/// // 音量：0~100，方向键一次 5，拖动吸附到整数。
/// let spec = Slider::new(0.0..=100.0).with_step(5.0).with_precision(0);
/// assert_eq!(spec.round(37.4), 37.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slider {
    start: f32,
    end: f32,
    step: Option<f32>,
    precision: Option<i32>,
    /// 朝向。
    pub orientation: Orientation,
    /// 点轨道的行为。
    pub track_click: TrackClick,
    /// 竖着时有多长（像素）。横着时不用——横的铺满一行。
    pub length: f32,
}

impl Default for Slider {
    fn default() -> Self {
        Self::new(0.0..=1.0)
    }
}

impl Slider {
    /// 指定值域。
    pub fn new(range: RangeInclusive<f32>) -> Self {
        let (start, end) = range.into_inner();
        Self {
            start,
            end,
            step: None,
            precision: None,
            orientation: Orientation::Auto,
            track_click: TrackClick::Drag,
            length: 120.0,
        }
    }

    /// 方向键一次走多远。
    ///
    /// 默认是全程的百分之一——按一百下走完，符合直觉。
    ///
    /// **和 `bevy_ui_widgets` 的默认值不同**：那边固定 1.0。对一条
    /// 0..=1 的滑条来说 1.0 意味着按一下方向键就从一头跳到另一头，
    /// 中间的值键盘永远够不着。
    pub fn with_step(mut self, step: f32) -> Self {
        self.step = Some(step);
        self
    }

    /// 拖动时吸附到小数点后几位。`0` 是取整，`-3` 是取到千位。
    ///
    /// 不设就是**不吸附**，拖出来是连续值。
    ///
    /// **和 `bevy_ui_widgets` 的默认值不同**：那边默认取整。对一条
    /// 0..=1 的滑条来说那意味着拖动只能拖出 0 和 1 两个值，看起来
    /// 就像滑条坏了。
    pub fn with_precision(mut self, decimals: i32) -> Self {
        self.precision = Some(decimals);
        self
    }

    /// 竖着放，`length` 像素长。值从**下往上**增，和音量条一致。
    pub fn vertical(mut self, length: f32) -> Self {
        self.orientation = Orientation::Vertical;
        self.length = length;
        self
    }

    /// 点轨道时的行为。
    pub fn with_track_click(mut self, track_click: TrackClick) -> Self {
        self.track_click = track_click;
        self
    }

    /// 值域下界。
    pub fn start(&self) -> f32 {
        self.start
    }

    /// 值域上界。
    pub fn end(&self) -> f32 {
        self.end
    }

    /// 值域有多宽。上界不大于下界时为 0。
    pub fn span(&self) -> f32 {
        (self.end - self.start).max(0.0)
    }

    /// 方向键一步有多大。
    pub fn step(&self) -> f32 {
        self.step.unwrap_or(self.span() / 100.0)
    }

    /// 夹进值域。
    pub fn clamp(&self, value: f32) -> f32 {
        if self.end <= self.start {
            // 值域塌成一个点（或者反了）。夹到下界，别让 `clamp` 因为
            // `min > max` 直接 panic——一个界面参数写反不该带走进程。
            self.start
        } else {
            value.clamp(self.start, self.end)
        }
    }

    /// 按精度吸附。没设精度时原样返回。
    pub fn round(&self, value: f32) -> f32 {
        match self.precision {
            Some(decimals) if decimals >= 0 => {
                let factor = 10f32.powi(decimals);
                (value * factor).round() / factor
            }
            // 负精度是「取到十位、百位、千位」。这里**先除后乘**，
            // 不是乘上 `10^负数` 再除：0.001 在二进制里不是精确值，
            // 乘除一轮之后取到千位的 1000 会变成 999.99994，显示成
            // 「999」——一个只该出现整千数的滑条上，这很扎眼。
            Some(decimals) => {
                let factor = 10f32.powi(-decimals);
                (value / factor).round() * factor
            }
            None => value,
        }
    }

    /// 值换算成 0..=1 的位置。值域塌成一个点时返回 0。
    pub fn fraction(&self, value: f32) -> f32 {
        let span = self.span();
        if span > 0.0 {
            ((value - self.start) / span).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// 0..=1 的位置换算回值。
    pub fn value_at(&self, fraction: f32) -> f32 {
        self.clamp(self.start + fraction.clamp(0.0, 1.0) * self.span())
    }
}

/// 一次拖动。
///
/// 记住**起点**而不是每帧往值上累加，是因为吸附：步子小于半个吸附单位
/// 时，每帧「加一点、吸附回去」的结果是值一动不动——滑条看着像卡住了，
/// 而鼠标明明在动。从起点加总位移就没有这个问题。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Drag {
    /// 按下那一刻的值。
    start_value: f32,
    /// 按下以来主轴上的总位移（像素）。
    offset: f32,
}

impl WidgetUi {
    /// 一条 0..=1 的滑条。
    pub fn slider(&mut self, id: &str, value: &mut f32, input: &UiInput) -> Id {
        self.slider_with(id, value, Slider::default(), input)
    }

    /// 一条自定值域、步进、朝向的滑条。
    ///
    /// **直接改传进来的 `value`**：拖动和方向键在声明期就应用了。
    ///
    /// ```no_run
    /// # use kui::UiInput;
    /// # use kui_widgets::{Slider, WidgetUi};
    /// # let mut w = WidgetUi::default();
    /// # let input = UiInput::default();
    /// # let mut volume = 50.0;
    /// w.slider_with("volume", &mut volume, Slider::new(0.0..=100.0).with_precision(0), &input);
    /// ```
    pub fn slider_with(&mut self, id: &str, value: &mut f32, spec: Slider, input: &UiInput) -> Id {
        let id = Id::new(id);
        if self.collapsed {
            return id;
        }

        // 上一帧的结果。矩形要等排版才知道，所以拖动一律用上一帧的——
        // 拖动是连续的，滞后一帧看不出来。
        let response = self.interaction.response(id);
        let vertical = spec.orientation.is_vertical(response.rect);

        // 值可能被外部改过（读档、重置面板）。先夹回值域，否则滑块
        // 会画到轨道外面去。
        *value = spec.clamp(*value);

        self.drag_slider(id, value, spec, input, &response, vertical);
        self.key_slider(id, value, spec, input, vertical);

        let fraction = spec.fraction(*value);
        let row = self.open_row;
        let grow = row.is_some() && self.row_first;
        if row.is_some() {
            self.row_first = false;
        }
        self.declared.push(Declared {
            id,
            widget: Widget::Slider {
                fraction,
                vertical,
                length: spec.length,
            },
            row,
            grow,
            tab_stop: true,
        });
        id
    }

    /// 指针：按下轨道、拖动。
    fn drag_slider(
        &mut self,
        id: Id,
        value: &mut f32,
        spec: Slider,
        input: &UiInput,
        response: &Response,
        vertical: bool,
    ) {
        if !response.held {
            // 松手了。留着的话下一次按下会被当成同一次拖动的延续，
            // 值会从上一次的起点重新算，滑块凭空跳一下。
            self.slider_drags.remove(&id);
            return;
        }

        let geometry = geometry(&self.theme, response.rect, spec.fraction(*value), vertical);

        // 这里不能用 `entry`：要插进去的 `start_value` 得等下面那段
        // 轨道点击跑完才定得下来（`Snap` 和 `Step` 都会改 `*value`），
        // 而那段还要读 `self.theme`——`entry` 会把整张表借住。
        #[allow(clippy::map_entry)]
        if !self.slider_drags.contains_key(&id) {
            // 按下的那一刻。按在滑块上还是轨道上，决定这一下做什么。
            let on_thumb = input.pointer.is_some_and(|p| geometry.thumb.contains(p));
            if !on_thumb && let Some(pointer) = input.pointer {
                match spec.track_click {
                    TrackClick::Drag => {}
                    TrackClick::Snap => {
                        *value = spec.round(spec.value_at(geometry.fraction_at(pointer)));
                    }
                    TrackClick::Step => {
                        // 朝点击的那一侧走一步。
                        let forward = geometry.fraction_at(pointer) > spec.fraction(*value);
                        let step = if forward { spec.step() } else { -spec.step() };
                        *value = spec.clamp(*value + step);
                    }
                }
            }
            self.slider_drags.insert(
                id,
                Drag {
                    start_value: *value,
                    offset: 0.0,
                },
            );
            return;
        }

        // 拖动中。主轴上的位移换算成值：竖着时**向上为增**，和音量条一致。
        let delta = if vertical {
            -response.drag.y
        } else {
            response.drag.x
        };
        let drag = self.slider_drags.get_mut(&id).expect("上面刚查过");
        drag.offset += delta;

        if geometry.travel > 0.0 {
            let moved = drag.offset / geometry.travel * spec.span();
            *value = spec.clamp(spec.round(drag.start_value + moved));
        }
    }

    /// 键盘：方向键加减一步，Home / End 到两端。
    fn key_slider(
        &mut self,
        id: Id,
        value: &mut f32,
        spec: Slider,
        input: &UiInput,
        _vertical: bool,
    ) {
        // 菜单开着时方向键归菜单。不让路的话在菜单里按方向键，
        // 底下那条滑条的值会跟着一起变。
        if self.menus_open() || self.interaction.focused() != Some(id) {
            return;
        }
        for key in &input.nav {
            // **横竖都认两组方向键**。只认主轴那一组的话，一条竖滑条
            // 按左右键毫无反应，而用户没有任何线索知道该按上下。
            *value = match key {
                NavKey::Left | NavKey::Down => spec.clamp(*value - spec.step()),
                NavKey::Right | NavKey::Up => spec.clamp(*value + spec.step()),
                NavKey::Home => spec.start(),
                NavKey::End => spec.end(),
                NavKey::Escape => *value,
            };
        }
    }
}

/// 轨道与滑块的几何。
///
/// 命中和绘制**共用这一份**。各算各的话，滑块边缘那一圈会出现「看着
/// 按在滑块上、实际按的是轨道」——于是 [`TrackClick::Snap`] 的滑条
/// 每次从边缘起手都会先跳一下。
pub(crate) struct Geometry {
    /// 轨道那条槽。
    pub(crate) track: Rect,
    /// 已填充的一段。
    pub(crate) filled: Rect,
    /// 滑块。
    pub(crate) thumb: Rect,
    /// 滑块**中心**能走的总行程（像素）。
    ///
    /// 是控件长度减掉一个滑块的粗细，不是控件长度：滑块中心走到头时
    /// 它的边缘正好贴着控件边缘。少减这一下，滑条推到两端时滑块会
    /// 探出去半个。
    pub(crate) travel: f32,
    /// 主轴的起点（滑块中心走到 0 时在哪）。
    origin: f32,
    /// 竖着放。
    vertical: bool,
}

impl Geometry {
    /// 指针落在这条滑条的哪个比例上（0..=1）。
    pub(crate) fn fraction_at(&self, pointer: Vec2) -> f32 {
        if self.travel <= 0.0 {
            return 0.0;
        }
        if self.vertical {
            // 竖着的从下往上增。
            ((self.origin - pointer.y) / self.travel).clamp(0.0, 1.0)
        } else {
            ((pointer.x - self.origin) / self.travel).clamp(0.0, 1.0)
        }
    }
}

/// 算一条滑条的轨道、填充段和滑块。
pub(crate) fn geometry(theme: &Theme, rect: Rect, fraction: f32, vertical: bool) -> Geometry {
    let thickness = 6.0;
    let radius = theme.row_height * 0.32;
    let center = rect.center();
    let size = rect.size();

    if vertical {
        let track = Rect {
            min: Vec2::new(center.x - thickness * 0.5, rect.min.y),
            max: Vec2::new(center.x + thickness * 0.5, rect.max.y),
        };
        let travel = (size.y - radius * 2.0).max(0.0);
        let origin = rect.max.y - radius;
        let knob_y = origin - travel * fraction;
        Geometry {
            track,
            // 填充从底部到滑块——满格是「满」，和音量条一致。
            filled: Rect {
                min: Vec2::new(track.min.x, knob_y),
                max: track.max,
            },
            thumb: Rect {
                min: Vec2::new(center.x - radius, knob_y - radius),
                max: Vec2::new(center.x + radius, knob_y + radius),
            },
            travel,
            origin,
            vertical,
        }
    } else {
        let track = Rect {
            min: Vec2::new(rect.min.x, center.y - thickness * 0.5),
            max: Vec2::new(rect.max.x, center.y + thickness * 0.5),
        };
        let travel = (size.x - radius * 2.0).max(0.0);
        let origin = rect.min.x + radius;
        let knob_x = origin + travel * fraction;
        Geometry {
            filled: Rect {
                min: track.min,
                max: Vec2::new(knob_x, track.max.y),
            },
            track,
            thumb: Rect {
                min: Vec2::new(knob_x - radius, center.y - radius),
                max: Vec2::new(knob_x + radius, center.y + radius),
            },
            travel,
            origin,
            vertical,
        }
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(_ui: &Ui, theme: &Theme, vertical: bool, length: f32) -> Vec2 {
    let thickness = theme.row_height * 0.64;
    if vertical {
        Vec2::new(thickness, length)
    } else {
        Vec2::new(120.0, theme.font_size)
    }
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    fraction: f32,
    vertical: bool,
) {
    let geometry = geometry(theme, rect, fraction, vertical);
    let round = geometry.track.size().min_element() * 0.5;

    ui.rounded_rect(geometry.track, round, theme.surface);
    if !geometry.filled.is_empty() {
        ui.rounded_rect(geometry.filled, round, theme.accent);
    }

    // 焦点框套在整条滑条外面，不是套在滑块上——套滑块的话它会跟着
    // 值到处跑，用户很难看出焦点在哪条滑条上。
    if response.focused {
        ui.border(rect.shrink(-2.0), theme.radius + 2.0, 2.0, theme.focus);
    }

    let fill = if response.held || response.hovered {
        Vec4::ONE
    } else {
        theme.text
    };
    ui.rounded_rect(
        geometry.thumb,
        geometry.thumb.size().min_element() * 0.5,
        fill,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{at, press, ui};
    use kui::PointerButton;

    /// 跑一帧。返回本帧之后的值。
    fn frame(w: &mut WidgetUi, ui: &mut kui::Ui, value: &mut f32, spec: Slider, input: &UiInput) {
        w.begin();
        w.slider_with("s", value, spec, input);
        w.finish(ui, input);
    }

    fn nav(key: NavKey) -> UiInput {
        UiInput {
            nav: vec![key],
            ..Default::default()
        }
    }

    #[test]
    fn the_thumb_stays_inside_the_track() {
        // 不夹的话，拖到两端时滑块会掉出去一半。
        for fraction in [0.0, 1.0] {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            let mut value = fraction;
            w.begin();
            let id = w.slider("s", &mut value, &UiInput::default());
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();

            let track = w.response(id).rect;
            for v in ui.draw_list().vertices() {
                assert!(
                    v.position[0] >= track.min.x - 0.01 && v.position[0] <= track.max.x + 0.01,
                    "fraction={fraction} 时滑块跑出了轨道：{}",
                    v.position[0]
                );
            }
        }
    }

    /// 值域两端都够得着。
    ///
    /// 这条防的是把行程算成「控件长度」而不是「控件长度减滑块粗细」：
    /// 那样的话滑块中心走到头时，它有一半在控件外面，而最后那一小段
    /// 值区间对应的位置根本点不到。
    #[test]
    fn both_ends_of_the_range_are_reachable() {
        let theme = Theme::default();
        let rect = Rect::new(0.0, 0.0, 200.0, 30.0);

        let low = geometry(&theme, rect, 0.0, false);
        let high = geometry(&theme, rect, 1.0, false);
        assert!(low.thumb.min.x >= rect.min.x - 0.01, "0 处滑块探出左边");
        assert!(high.thumb.max.x <= rect.max.x + 0.01, "1 处滑块探出右边");
        assert!(high.thumb.min.x > low.thumb.min.x, "两端算出来是同一个位置");
    }

    /// 点在轨道上取到的比例，和该比例画出的滑块中心对得上。
    #[test]
    fn clicking_the_track_maps_back_to_the_same_position() {
        let theme = Theme::default();
        let rect = Rect::new(0.0, 0.0, 200.0, 30.0);
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let geometry = geometry(&theme, rect, fraction, false);
            let center = geometry.thumb.center();
            let back = geometry.fraction_at(center);
            assert!(
                (back - fraction).abs() < 0.001,
                "{fraction} 画在 {center:?}，点回去成了 {back}"
            );
        }
    }

    #[test]
    fn a_vertical_slider_counts_upward_from_the_bottom() {
        let theme = Theme::default();
        let rect = Rect::new(0.0, 0.0, 30.0, 200.0);
        let low = geometry(&theme, rect, 0.0, true);
        let high = geometry(&theme, rect, 1.0, true);
        assert!(
            high.thumb.center().y < low.thumb.center().y,
            "竖滑条该是往上增：0 在 {:?}，1 在 {:?}",
            low.thumb.center(),
            high.thumb.center()
        );
    }

    #[test]
    fn the_range_maps_to_the_declared_bounds() {
        let spec = Slider::new(20.0..=80.0);
        assert_eq!(spec.fraction(20.0), 0.0);
        assert_eq!(spec.fraction(50.0), 0.5);
        assert_eq!(spec.fraction(80.0), 1.0);
        assert_eq!(spec.value_at(0.5), 50.0);
    }

    /// 值域反着写（上界比下界小）不该 panic。
    ///
    /// `f32::clamp` 在 `min > max` 时直接 panic——一个界面参数写反
    /// 不该把整个进程带走。
    #[test]
    fn a_backwards_range_does_not_panic() {
        let spec = Slider::new(10.0..=0.0);
        assert_eq!(spec.clamp(5.0), 10.0);
        assert_eq!(spec.span(), 0.0);
        assert_eq!(spec.fraction(5.0), 0.0);
    }

    #[test]
    fn precision_snaps_the_value() {
        let spec = Slider::new(0.0..=100.0).with_precision(0);
        assert_eq!(spec.round(37.4), 37.0);
        assert_eq!(spec.round(37.6), 38.0);

        let coarse = Slider::new(0.0..=10000.0).with_precision(-3);
        assert_eq!(coarse.round(1400.0), 1000.0);
        assert_eq!(coarse.round(1600.0), 2000.0);
    }

    #[test]
    fn no_precision_means_continuous() {
        let spec = Slider::new(0.0..=1.0);
        assert_eq!(spec.round(0.123_456), 0.123_456);
    }

    #[test]
    fn the_default_step_walks_the_range_in_a_hundred_presses() {
        assert_eq!(Slider::new(0.0..=100.0).step(), 1.0);
        assert_eq!(Slider::new(0.0..=1.0).step(), 0.01);
    }

    /// 方向键调值，两组方向键都认。
    #[test]
    fn arrow_keys_step_the_value() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 50.0;

        // 先用 Tab 走到它。
        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        frame(&mut w, &mut ui, &mut value, spec, &tab);

        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Right));
        assert_eq!(value, 51.0);
        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Left));
        assert_eq!(value, 50.0);
        // 横滑条也认上下——只认一组的话，用户没有线索知道该按哪个。
        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Up));
        assert_eq!(value, 51.0);
        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Down));
        assert_eq!(value, 50.0);
    }

    #[test]
    fn home_and_end_jump_to_the_extremes() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 50.0;

        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        frame(&mut w, &mut ui, &mut value, spec, &tab);

        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Home));
        assert_eq!(value, 0.0);
        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::End));
        assert_eq!(value, 100.0);
    }

    #[test]
    fn stepping_stops_at_the_ends() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 100.0;

        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        frame(&mut w, &mut ui, &mut value, spec, &tab);
        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Right));
        assert_eq!(value, 100.0, "到顶了还该是 100");
    }

    /// 没焦点的滑条不该跟着别人的方向键动。
    #[test]
    fn an_unfocused_slider_ignores_the_arrow_keys() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 50.0;

        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        frame(&mut w, &mut ui, &mut value, spec, &nav(NavKey::Right));
        assert_eq!(value, 50.0);
    }

    /// 空格不该激活滑条。
    ///
    /// 滑条**能**拿焦点（要用方向键调值），但回车 / 空格在它身上没有
    /// 意义。认了的话，一个「点击 = 复位」之类的语义会凭空冒出来。
    #[test]
    fn the_activate_key_does_nothing_on_a_slider() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 50.0;

        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        frame(&mut w, &mut ui, &mut value, spec, &tab);

        let mut input = UiInput {
            activate: true,
            ..Default::default()
        };
        input.nav.clear();
        frame(&mut w, &mut ui, &mut value, spec, &input);
        assert_eq!(value, 50.0);
        assert!(!w.response(Id::new("s")).clicked, "滑条不认激活键");
    }

    /// 点在轨道上，`Snap` 直接跳过去。
    #[test]
    fn track_click_snap_jumps_to_the_pointer() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0).with_track_click(TrackClick::Snap);
        let mut value = 0.0;

        // 第一帧排版，拿到矩形。
        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        let rect = w.response(Id::new("s")).rect;
        let target = rect.center();

        // 按在正中间。第一帧 held 还没算出来，所以要两帧。
        frame(
            &mut w,
            &mut ui,
            &mut value,
            spec,
            &press(target.x, target.y),
        );
        frame(&mut w, &mut ui, &mut value, spec, &at(target.x, target.y));

        assert!(
            (value - 50.0).abs() < 2.0,
            "点在正中间该跳到 50 附近，得到 {value}"
        );
    }

    /// `Drag` 模式下点轨道不改值。
    #[test]
    fn track_click_drag_leaves_the_value_alone() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 0.0;

        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        let rect = w.response(Id::new("s")).rect;
        let target = rect.center();

        frame(
            &mut w,
            &mut ui,
            &mut value,
            spec,
            &press(target.x, target.y),
        );
        frame(&mut w, &mut ui, &mut value, spec, &at(target.x, target.y));
        assert_eq!(value, 0.0, "Drag 模式点轨道不该改值");
    }

    /// `Step` 模式下点轨道走一步。
    #[test]
    fn track_click_step_moves_one_step() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0)
            .with_step(10.0)
            .with_track_click(TrackClick::Step);
        let mut value = 50.0;

        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        let rect = w.response(Id::new("s")).rect;
        // 点在靠右的地方 = 往上走一步。
        let target = Vec2::new(rect.max.x - 10.0, rect.center().y);

        frame(
            &mut w,
            &mut ui,
            &mut value,
            spec,
            &press(target.x, target.y),
        );
        frame(&mut w, &mut ui, &mut value, spec, &at(target.x, target.y));
        assert_eq!(value, 60.0);
    }

    /// 拖动跟手：拖过半个轨道，值走半个值域。
    #[test]
    fn dragging_moves_the_value_with_the_pointer() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 0.0;

        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        let rect = w.response(Id::new("s")).rect;
        let travel = geometry(&w.theme, rect, 0.0, false).travel;
        let start = rect.min.x + 5.0;

        // 按住最左端。
        frame(
            &mut w,
            &mut ui,
            &mut value,
            spec,
            &press(start, rect.center().y),
        );
        // 往右拖半个行程。
        frame(
            &mut w,
            &mut ui,
            &mut value,
            spec,
            &at(start + travel * 0.5, rect.center().y),
        );
        frame(
            &mut w,
            &mut ui,
            &mut value,
            spec,
            &at(start + travel * 0.5, rect.center().y),
        );

        assert!(
            (value - 50.0).abs() < 2.0,
            "拖过半个行程该到 50 附近，得到 {value}"
        );
    }

    /// 吸附得很粗时，慢慢拖仍然拖得动。
    ///
    /// 这条防的是「每帧往值上加增量再吸附」：一帧走的距离不足半个吸附
    /// 单位时会被原样吸附回去，值永远不动——鼠标在动，滑条卡着。
    #[test]
    fn a_slow_drag_still_moves_a_coarsely_snapped_slider() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0).with_precision(0);
        let mut value = 0.0;

        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        let rect = w.response(Id::new("s")).rect;
        let y = rect.center().y;
        let mut x = rect.min.x + 5.0;

        frame(&mut w, &mut ui, &mut value, spec, &press(x, y));
        // 每帧只挪一像素，走 40 帧。
        for _ in 0..40 {
            x += 1.0;
            frame(&mut w, &mut ui, &mut value, spec, &at(x, y));
        }

        assert!(value > 5.0, "慢拖被吸附卡住了，值还是 {value}");
    }

    /// 松手之后再按下，是一次新的拖动。
    ///
    /// 不清掉旧的拖动状态的话，第二次按下会接着上一次的起点算，
    /// 滑块会凭空跳回去。
    #[test]
    fn releasing_ends_the_drag() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let spec = Slider::new(0.0..=100.0);
        let mut value = 0.0;

        frame(&mut w, &mut ui, &mut value, spec, &UiInput::default());
        let rect = w.response(Id::new("s")).rect;
        let y = rect.center().y;
        let x = rect.min.x + 5.0;

        frame(&mut w, &mut ui, &mut value, spec, &press(x, y));
        frame(&mut w, &mut ui, &mut value, spec, &at(x + 40.0, y));

        // 松手。
        let mut release = at(x + 40.0, y);
        release.released.push(PointerButton::Primary);
        frame(&mut w, &mut ui, &mut value, spec, &release);
        let after = value;

        // 指针继续动，但没按着——值不该跟着走。
        frame(&mut w, &mut ui, &mut value, spec, &at(x + 120.0, y));
        assert_eq!(value, after, "松手之后值还在跟着指针跑");
    }
}
