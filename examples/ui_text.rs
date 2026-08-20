//! 文字与 UI 绘制。
//!
//! ```bash
//! cargo run --example ui_text
//! ```
//!
//! 左右方向键改字号，空格切换换行策略。
//!
//! # 目前有什么
//!
//! 即时模式的**绘制层**：矩形、圆角、边框、裁剪、文字。
//! 布局（taffy）、事件路由、控件**还没做**——所以这个例子里的「按钮」
//! 是手摆坐标画出来的，点不了。
//!
//! # 引擎不自带字体
//!
//! 覆盖中文的字体动辄十几兆，塞进仓库不合适。这里用 [`kfont::system_font`]
//! 去几个常见位置找一个；正经项目应当把字体当资源一起发布——
//! 系统装了什么不受控，同一份界面在两台机器上可能宽度都不一样。

use kengine::prelude::*;

/// 面板背景。
const PANEL: Vec4 = Vec4::new(0.10, 0.11, 0.14, 0.92);
/// 主色。
const ACCENT: Vec4 = Vec4::new(0.25, 0.55, 1.0, 1.0);
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
}

impl Default for UiDemo {
    fn default() -> Self {
        Self {
            size: 18.0,
            wrap: TextWrap::Word,
            ready: false,
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

    /// 画一个**看起来像**按钮的东西。
    ///
    /// 只是画，点不了——事件路由还没做。写在这里是为了让圆角、边框、
    /// 居中文字这几样一起过一遍。
    fn fake_button(&self, ui: &mut Ui, rect: UiRect, label: &str, active: bool) {
        let (fill, text) = if active {
            (ACCENT, Vec4::ONE)
        } else {
            (Vec4::new(0.18, 0.19, 0.24, 1.0), DIM)
        };
        ui.rounded_rect(rect, 6.0, fill);
        ui.border(rect, 6.0, 1.0, Vec4::new(1.0, 1.0, 1.0, 0.15));
        ui.text_centered(
            rect,
            label,
            &TextStyle {
                size: 15.0,
                ..Default::default()
            },
            text,
        );
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

        klog::info!("← → 改字号，空格切换换行策略，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        // TEMP-VALIDATION
        if ctx.elapsed > 2.0 && ctx.elapsed < 2.1 {
            klog::info!(
                "UI 顶点 {} / 绘制调用 {} / 图集版本 {}",
                ctx.stats.ui_vertices,
                ctx.stats.draw_calls,
                ctx.ui.atlas_version()
            );
        }
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

        let screen = ctx.ui.screen();
        let panel = UiRect::new(24.0, 24.0, (screen.x - 48.0).min(560.0), 300.0);

        ctx.ui.rounded_rect(panel, 10.0, PANEL);
        ctx.ui
            .border(panel, 10.0, 1.0, Vec4::new(1.0, 1.0, 1.0, 0.12));

        let inner = panel.shrink(18.0);
        let title = ctx.ui.text(
            inner.min,
            "kengine 文字渲染",
            &TextStyle {
                size: 22.0,
                ..Default::default()
            },
            TEXT,
            None,
        );

        // 正文限宽在面板内。裁剪是必须的：换行策略切到 None 时，
        // 这段文字会一路画出面板外面去。
        let body_top = inner.min.y + title.size.y + 12.0;
        let body = UiRect::from_corners(
            Vec2::new(inner.min.x, body_top),
            Vec2::new(inner.max.x, inner.max.y - 44.0),
        );
        ctx.ui.push_clip(body);
        ctx.ui
            .text(body.min, SAMPLE, &self.style(), TEXT, Some(body.size().x));
        ctx.ui.pop_clip();

        // 底部一排「按钮」，展示圆角与居中文字。
        let labels = ["换行", "省略号", "不换行"];
        let current = match self.wrap {
            TextWrap::Word => 0,
            TextWrap::Ellipsis => 1,
            TextWrap::None => 2,
        };
        for (i, label) in labels.iter().enumerate() {
            let rect = UiRect::new(
                inner.min.x + i as f32 * 96.0,
                inner.max.y - 32.0,
                88.0,
                32.0,
            );
            self.fake_button(ctx.ui, rect, label, i == current);
        }

        // 右上角的状态行。
        let status = format!(
            "{:.0} px · {} 绘制调用 · 图集版本 {}",
            self.size,
            ctx.stats.draw_calls,
            ctx.ui.atlas_version(),
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
