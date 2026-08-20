//! 文字与 UI 绘制。
//!
//! ```bash
//! cargo run --example ui_text
//! ```
//!
//! 上面几个控件是**能点的**；左右方向键改字号。
//!
//! 上半屏是**能点的控件**（taffy 布局 + 事件路由）；
//! 下半屏是手摆坐标的文字排版展示。
//!
//! # 控件为什么滞后一帧
//!
//! 一个按钮是不是「悬停」取决于它的矩形，而矩形要等整棵树排完才知道。
//! 所以流程是「声明 → 求解 → 绘制」，`response()` 查到的是上一次
//! `finish()` 之后的结果。对 HUD 与菜单看不出来。
//!
//! # 引擎不自带字体
//!
//! 覆盖中文的字体动辄十几兆，塞进仓库不合适。这里用 [`kfont::system_font`]
//! 去几个常见位置找一个；正经项目应当把字体当资源一起发布——
//! 系统装了什么不受控，同一份界面在两台机器上可能宽度都不一样。

use kengine::prelude::*;

/// 面板背景。
const PANEL: Vec4 = Vec4::new(0.10, 0.11, 0.14, 0.92);
/// 正文颜色。
const TEXT: Vec4 = Vec4::new(0.92, 0.93, 0.96, 1.0);
/// 次要文字。
const DIM: Vec4 = Vec4::new(0.55, 0.58, 0.66, 1.0);

const SAMPLE: &str = "中英混排 mixed text。这一段用来看断行对不对——\
中文按字断，英文按空格断，句号和逗号不会跑到行首。\
A supercalifragilisticexpialidocious word must be broken by force.";

struct UiDemo {
    size: f32,
    wrap: TextWrap,
    ready: bool,
    /// 控件层。跨帧保存交互状态（悬停、按下、焦点）。
    widgets: WidgetUi,
    /// 控件的状态由调用方保存——控件自己不存，见 `WidgetUi::checkbox`。
    show_body: bool,
    volume: f32,
    clicks: u32,
}

impl Default for UiDemo {
    fn default() -> Self {
        Self {
            size: 18.0,
            wrap: TextWrap::Word,
            ready: false,
            widgets: WidgetUi::default(),
            show_body: true,
            volume: 0.7,
            clicks: 0,
        }
    }
}

impl UiDemo {
    fn style(&self) -> TextStyle {
        TextStyle {
            size: self.size,
            line_height: 1.35,
            wrap: self.wrap,
            ..Default::default()
        }
    }
}

impl Plugin for UiDemo {
    fn init(&mut self, ctx: &mut Context) {
        // 场景里放点东西，好看出 UI 确实画在 3D 之上。
        ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 1.5, 4.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(3.0))
                .with_transform(Transform::looking_at(
                    Vec3::new(2.0, 4.0, 3.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::new(0.9, 0.5, 0.2), 0.4)),
        );

        match kengine::kfont::system_font() {
            Some(path) => match Font::from_file(&path) {
                Ok(font) => {
                    ctx.ui.add_font(font);
                    self.ready = true;
                    klog::info!("字体：{}", path.display());
                }
                Err(e) => klog::error!("字体加载失败：{e}"),
            },
            None => klog::error!("本机找不到可用的系统字体，界面上不会有文字"),
        }

        klog::info!("← → 改字号，Tab 切焦点，鼠标点控件，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
        if ctx.input.key_just_pressed(KeyCode::ArrowRight) {
            self.size = (self.size + 2.0).min(48.0);
        }
        if ctx.input.key_just_pressed(KeyCode::ArrowLeft) {
            self.size = (self.size - 2.0).max(10.0);
        }
        if ctx.input.key_just_pressed(KeyCode::Space) {
            self.wrap = match self.wrap {
                TextWrap::Word => TextWrap::Ellipsis,
                TextWrap::Ellipsis => TextWrap::None,
                TextWrap::None => TextWrap::Word,
            };
        }

        if !self.ready {
            return;
        }

        // ── 上半屏：能点的控件 ──
        //
        // 声明 → 求解 → 绘制。`finish` 里一次性排版、判交互、出几何。
        self.widgets.begin();

        self.widgets.label("title", "kengine UI");
        self.widgets
            .dim_label("hint", format!("按钮被点了 {} 次", self.clicks));

        let toggle = self.widgets.button("toggle", "切换换行策略");
        let reset = self.widgets.button("reset", "重置字号");
        let body_box = self.widgets.checkbox("body", "显示正文", self.show_body);
        let volume = self.widgets.slider("volume", self.volume);

        self.widgets.finish(ctx.ui, ctx.ui_input);

        // 读上一帧的交互结果。
        if self.widgets.response(toggle).clicked {
            self.clicks += 1;
            self.wrap = match self.wrap {
                TextWrap::Word => TextWrap::Ellipsis,
                TextWrap::Ellipsis => TextWrap::None,
                TextWrap::None => TextWrap::Word,
            };
        }
        if self.widgets.response(reset).clicked {
            self.clicks += 1;
            self.size = 18.0;
        }
        if self.widgets.response(body_box).clicked {
            self.show_body = !self.show_body;
        }
        // 滑条不存状态，拖动量要自己折算成值。
        let slider = self.widgets.response(volume);
        if slider.held && slider.rect.size().x > 0.0 {
            self.volume = (self.volume + slider.drag.x / slider.rect.size().x).clamp(0.0, 1.0);
        }

        // ── 下半屏：文字排版 ──
        if !self.show_body {
            return;
        }
        let screen = ctx.ui.screen();
        let panel = UiRect::new(
            24.0,
            screen.y * 0.5,
            (screen.x - 48.0).min(560.0),
            screen.y * 0.5 - 24.0,
        );
        ctx.ui.rounded_rect(panel, 10.0, PANEL);
        ctx.ui
            .border(panel, 10.0, 1.0, Vec4::new(1.0, 1.0, 1.0, 0.12));

        let inner = panel.shrink(18.0);
        // 裁剪是必须的：换行策略切到「不换行」时，这段文字会一路画到
        // 面板外面去。
        ctx.ui.push_clip(inner);
        ctx.ui
            .text(inner.min, SAMPLE, &self.style(), TEXT, Some(inner.size().x));
        ctx.ui.pop_clip();

        // 右上角状态行。
        let status = format!(
            "{:.0} px · 音量 {:.0}% · UI {} 顶点",
            self.size,
            self.volume * 100.0,
            ctx.stats.ui_vertices,
        );
        let style = TextStyle {
            size: 13.0,
            ..Default::default()
        };
        let measured = ctx.ui.measure(&status, &style, None);
        ctx.ui.text(
            Vec2::new(screen.x - measured.size.x - 16.0, 16.0),
            &status,
            &style,
            DIM,
            None,
        );
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — UI & text")
        .add_plugin(UiDemo::default())
        .run();
}
