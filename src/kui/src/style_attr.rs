//! 把标记里的属性变成样式。
//!
//! ```xml
//! <node direction="row" padding="12 8" gap="6" background="#1a1a1fee">
//!     <text size="18" color="#fff">标题</text>
//!     <button hover:background="#3a7bd5">确定</button>
//! </node>
//! ```
//!
//! # 两类属性
//!
//! - **布局**：`direction`、`padding`、`width`……落到 [`Style`]，交给 taffy。
//! - **外观**：`background`、`color`、`radius`……落到 [`Visual`]，绘制时用。
//!
//! 分开是因为它们的生命周期不同：布局属性一变要重新求解整棵树，
//! 外观属性只影响这一帧怎么画。混在一起的话，改个颜色也会触发重排。
//!
//! # `hover:` 前缀
//!
//! 属性名带前缀就只在那个状态下生效：
//!
//! ```xml
//! <button background="#333" hover:background="#555" pressed:background="#222"/>
//! ```
//!
//! 前缀在这里只负责**解析出来**，具体哪个状态生效由控件层决定——
//! 核心层不知道「悬停」是什么，它只做命中测试。
//!
//! # 不认识的属性只警告，不报错
//!
//! 模板是外部文件，会比引擎更新得快。一个拼错的属性名让整个界面起不来
//! 是不划算的——那个界面里别的东西本来都是好的。

use crate::layout::{AlignCross, Direction, Edges, Justify, Length, Style};
use kmath::{Vec2, Vec4};
use std::fmt;

/// 一个节点的外观，绘制时用。
///
/// 每一项都是 [`Option`]：没写的属性该**继承或用控件的默认值**，
/// 而不是被一个「零值」盖掉。写死默认值的话，模板里不写 `color`
/// 就等于把文字设成了纯黑。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Visual {
    /// 底色。
    pub background: Option<Vec4>,
    /// 文字颜色。
    pub color: Option<Vec4>,
    /// 字号。
    pub font_size: Option<f32>,
    /// 圆角半径。
    pub radius: Option<f32>,
    /// 边框宽度。
    pub border_width: Option<f32>,
    /// 边框颜色。
    pub border_color: Option<Vec4>,
}

impl Visual {
    /// 把 `other` 里写了的项盖到自己身上，没写的保持不变。
    ///
    /// 状态样式（`hover:`）就是这么叠在基础样式上的：只覆盖写了的那几项。
    pub fn overlay(&mut self, other: &Visual) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field;
                }
            };
        }
        take!(background);
        take!(color);
        take!(font_size);
        take!(radius);
        take!(border_width);
        take!(border_color);
    }
}

/// 属性名上的状态前缀。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Selector {
    /// 没有前缀，任何状态下都生效。
    Base,
    /// `hover:`——指针悬在上面。
    Hover,
    /// `pressed:`——按下去还没松。
    Pressed,
}

impl Selector {
    /// 从属性名里剥掉前缀，返回状态与剩下的名字。
    ///
    /// 不认识的前缀**当成名字的一部分**，交给上层去报「未知属性」——
    /// 在这里报「未知前缀」的话，`foo:bar` 会得到两条不同的错误信息，
    /// 而它们说的是同一件事。
    pub fn split(name: &str) -> (Selector, &str) {
        match name.split_once(':') {
            Some(("hover", rest)) => (Selector::Hover, rest),
            Some(("pressed", rest)) => (Selector::Pressed, rest),
            _ => (Selector::Base, name),
        }
    }
}

/// 属性解析失败。
#[derive(Debug, Clone, PartialEq)]
pub struct StyleError {
    /// 属性名。
    pub attribute: String,
    /// 出错的值。
    pub value: String,
    /// 出了什么事。
    pub message: String,
}

impl fmt::Display for StyleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "属性 `{}` 的值 `{}` 有问题：{}",
            self.attribute, self.value, self.message
        )
    }
}

impl std::error::Error for StyleError {}

/// 一条属性应用之后的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// 认识并且应用了。
    Ok,
    /// 不是样式属性（比如 `id`、`on_press`），上层自己处理。
    NotStyle,
}

/// 把一条属性应用到样式上。
///
/// 不认识的属性返回 [`Applied::NotStyle`]，**不报错**——上层可能认识它
/// （`id`、`on_press`、`name` 都不是样式）。
pub fn apply(
    name: &str,
    value: &str,
    layout: &mut Style,
    visual: &mut Visual,
) -> Result<Applied, StyleError> {
    let fail = |message: &str| StyleError {
        attribute: name.to_string(),
        value: value.to_string(),
        message: message.to_string(),
    };

    match name {
        // ── 布局 ──
        "direction" => {
            layout.direction = parse_direction(value).ok_or_else(|| fail("要是 row 或 column"))?
        }
        "justify" => {
            layout.justify = parse_justify(value)
                .ok_or_else(|| fail("要是 start / center / end / space_between / space_around"))?
        }
        "align" => {
            layout.align =
                parse_align(value).ok_or_else(|| fail("要是 start / center / end / stretch"))?
        }
        "width" => {
            layout.width = parse_length(value).ok_or_else(|| fail("要是 auto、像素数或百分比"))?
        }
        "height" => {
            layout.height = parse_length(value).ok_or_else(|| fail("要是 auto、像素数或百分比"))?
        }
        "min_width" => layout.min_size.x = parse_f32(value).ok_or_else(|| fail("要是一个数"))?,
        "min_height" => layout.min_size.y = parse_f32(value).ok_or_else(|| fail("要是一个数"))?,
        "min_size" => {
            let edges = parse_edges(value).ok_or_else(|| fail("要是一到四个数"))?;
            layout.min_size = Vec2::new(edges.left, edges.top);
        }
        "padding" => layout.padding = parse_edges(value).ok_or_else(|| fail("要是一到四个数"))?,
        "margin" => layout.margin = parse_edges(value).ok_or_else(|| fail("要是一到四个数"))?,
        "gap" => layout.gap = parse_f32(value).ok_or_else(|| fail("要是一个数"))?,
        "grow" => layout.grow = parse_f32(value).ok_or_else(|| fail("要是一个数"))?,
        "shrink" => layout.shrink = parse_f32(value).ok_or_else(|| fail("要是一个数"))?,

        // ── 外观 ──
        "background" => {
            visual.background =
                Some(parse_color(value).ok_or_else(|| fail("要是 #rgb / #rrggbb / #rrggbbaa"))?)
        }
        "color" => {
            visual.color =
                Some(parse_color(value).ok_or_else(|| fail("要是 #rgb / #rrggbb / #rrggbbaa"))?)
        }
        "size" | "font_size" => {
            visual.font_size = Some(parse_f32(value).ok_or_else(|| fail("要是一个数"))?)
        }
        "radius" => visual.radius = Some(parse_f32(value).ok_or_else(|| fail("要是一个数"))?),
        "border" => visual.border_width = Some(parse_f32(value).ok_or_else(|| fail("要是一个数"))?),
        "border_color" => {
            visual.border_color =
                Some(parse_color(value).ok_or_else(|| fail("要是 #rgb / #rrggbb / #rrggbbaa"))?)
        }

        _ => return Ok(Applied::NotStyle),
    }
    Ok(Applied::Ok)
}

/// 解析一个长度：`auto`、`12`、`12px`、`50%`。
///
/// 不带单位的数字当**像素**——模板里绝大多数长度是像素，
/// 强制写 `px` 只是噪音。
pub fn parse_length(value: &str) -> Option<Length> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        return Some(Length::Auto);
    }
    if let Some(number) = value.strip_suffix('%') {
        // 百分比在内部是 0..=1，模板里写 0..=100 更符合直觉。
        return parse_f32(number).map(|v| Length::Percent(v / 100.0));
    }
    let number = value.strip_suffix("px").unwrap_or(value);
    parse_f32(number).map(Length::Px)
}

/// 解析一个数。拒绝 NaN 与无穷——它们会让 taffy 算出一整棵 NaN 的布局，
/// 界面整个消失而且看不出原因。
pub fn parse_f32(value: &str) -> Option<f32> {
    let parsed: f32 = value.trim().parse().ok()?;
    parsed.is_finite().then_some(parsed)
}

/// 解析边距，支持 CSS 的简写：
///
/// - `12` —— 四边都是 12
/// - `12 8` —— 上下 12、左右 8
/// - `12 8 4 2` —— 上、右、下、左（和 CSS 一样顺时针）
pub fn parse_edges(value: &str) -> Option<Edges> {
    let parts: Vec<f32> = value
        .split_whitespace()
        .map(parse_f32)
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [all] => Some(Edges::all(*all)),
        [vertical, horizontal] => Some(Edges {
            top: *vertical,
            bottom: *vertical,
            left: *horizontal,
            right: *horizontal,
        }),
        [top, right, bottom, left] => Some(Edges {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        }),
        _ => None,
    }
}

/// 解析颜色：`#rgb`、`#rgba`、`#rrggbb`、`#rrggbbaa`。
///
/// # 转成线性空间
///
/// 十六进制颜色是**取色器里看到的那个值**，也就是 sRGB。直接除以 255
/// 当线性值用的话，画出来会明显偏亮，而且和设计稿对不上。
///
/// alpha **不转**——它是混合系数，不是颜色。
pub fn parse_color(value: &str) -> Option<Vec4> {
    let hex = value.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    // 三位和四位是每通道一位的简写：`#f0c` 等于 `#ff00cc`。
    let expand = |c: char| -> u8 {
        let v = c.to_digit(16).unwrap_or(0) as u8;
        v * 16 + v
    };
    let bytes: Vec<u8> = match hex.len() {
        3 | 4 => hex.chars().map(expand).collect(),
        6 | 8 => (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect(),
        _ => return None,
    };

    let srgb_to_linear = |v: u8| -> f32 {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };

    Some(Vec4::new(
        srgb_to_linear(bytes[0]),
        srgb_to_linear(bytes[1]),
        srgb_to_linear(bytes[2]),
        // alpha 是混合系数，不是颜色，不做 sRGB 转换。
        bytes.get(3).map_or(1.0, |a| *a as f32 / 255.0),
    ))
}

fn parse_direction(value: &str) -> Option<Direction> {
    match value.trim() {
        "row" => Some(Direction::Row),
        "column" => Some(Direction::Column),
        _ => None,
    }
}

fn parse_justify(value: &str) -> Option<Justify> {
    match value.trim() {
        "start" => Some(Justify::Start),
        "center" => Some(Justify::Center),
        "end" => Some(Justify::End),
        "space_between" => Some(Justify::SpaceBetween),
        "space_around" => Some(Justify::SpaceAround),
        _ => None,
    }
}

fn parse_align(value: &str) -> Option<AlignCross> {
    match value.trim() {
        "start" => Some(AlignCross::Start),
        "center" => Some(AlignCross::Center),
        "end" => Some(AlignCross::End),
        "stretch" => Some(AlignCross::Stretch),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_one(name: &str, value: &str) -> (Style, Visual) {
        let mut layout = Style::default();
        let mut visual = Visual::default();
        apply(name, value, &mut layout, &mut visual).expect("该能解析");
        (layout, visual)
    }

    // ── 长度 ──

    #[test]
    fn a_bare_number_means_pixels() {
        // 模板里绝大多数长度是像素，强制写 px 只是噪音。
        assert_eq!(parse_length("12"), Some(Length::Px(12.0)));
        assert_eq!(parse_length("12px"), Some(Length::Px(12.0)));
        assert_eq!(parse_length(" 12 "), Some(Length::Px(12.0)));
    }

    #[test]
    fn percentages_are_written_as_0_to_100() {
        // 内部是 0..=1，模板里写 0..=100 更符合直觉。
        assert_eq!(parse_length("50%"), Some(Length::Percent(0.5)));
        assert_eq!(parse_length("100%"), Some(Length::Percent(1.0)));
    }

    #[test]
    fn auto_is_case_insensitive() {
        assert_eq!(parse_length("auto"), Some(Length::Auto));
        assert_eq!(parse_length("AUTO"), Some(Length::Auto));
    }

    #[test]
    fn a_bogus_length_is_rejected() {
        for value in ["", "abc", "12pt", "%"] {
            assert_eq!(parse_length(value), None, "`{value}` 本该被拒");
        }
    }

    #[test]
    fn nan_and_infinity_are_rejected() {
        // 它们会让 taffy 算出一整棵 NaN 的布局，界面整个消失，
        // 而且看不出原因。
        for value in ["NaN", "nan", "inf", "-inf", "infinity"] {
            assert_eq!(parse_f32(value), None, "`{value}` 本该被拒");
        }
    }

    // ── 边距 ──

    #[test]
    fn one_number_covers_all_four_edges() {
        let edges = parse_edges("12").unwrap();
        assert_eq!(
            (edges.top, edges.right, edges.bottom, edges.left),
            (12.0, 12.0, 12.0, 12.0)
        );
    }

    #[test]
    fn two_numbers_are_vertical_then_horizontal() {
        let edges = parse_edges("12 8").unwrap();
        assert_eq!(edges.top, 12.0);
        assert_eq!(edges.bottom, 12.0);
        assert_eq!(edges.left, 8.0);
        assert_eq!(edges.right, 8.0);
    }

    #[test]
    fn four_numbers_go_clockwise_from_the_top() {
        // 和 CSS 一样：上、右、下、左。顺序不同的话，从网页抄过来的
        // 数值会静默地变成另一个样子。
        let edges = parse_edges("1 2 3 4").unwrap();
        assert_eq!(
            (edges.top, edges.right, edges.bottom, edges.left),
            (1.0, 2.0, 3.0, 4.0)
        );
    }

    #[test]
    fn three_numbers_are_rejected() {
        // CSS 里三个值有含义（上 / 左右 / 下），但那条规则很少有人记得住。
        // 与其猜错，不如报错。
        assert_eq!(parse_edges("1 2 3"), None);
        assert_eq!(parse_edges(""), None);
        assert_eq!(parse_edges("1 2 3 4 5"), None);
    }

    // ── 颜色 ──

    #[test]
    fn hex_colors_are_converted_to_linear() {
        // 十六进制颜色是取色器里看到的那个值（sRGB）。直接除以 255
        // 当线性值用的话，画出来明显偏亮。
        let white = parse_color("#ffffff").unwrap();
        assert!((white.x - 1.0).abs() < 1e-5);

        let mid = parse_color("#808080").unwrap();
        // sRGB 的 0.5 在线性空间里约 0.216，不是 0.5。
        assert!(
            (mid.x - 0.2158).abs() < 0.01,
            "没做 sRGB 转换？实测 {}",
            mid.x
        );
    }

    #[test]
    fn alpha_is_not_gamma_corrected() {
        // alpha 是混合系数，不是颜色。当颜色转的话，半透明会明显偏透。
        let color = parse_color("#00000080").unwrap();
        assert!((color.w - 0.502).abs() < 0.01, "alpha 是 {}", color.w);
    }

    #[test]
    fn short_hex_expands_each_digit() {
        // `#f0c` 等于 `#ff00cc`。
        assert_eq!(parse_color("#f0c"), parse_color("#ff00cc"));
        assert_eq!(parse_color("#f0c8"), parse_color("#ff00cc88"));
    }

    #[test]
    fn colors_default_to_opaque() {
        assert_eq!(parse_color("#123456").unwrap().w, 1.0);
    }

    #[test]
    fn a_bogus_color_is_rejected() {
        for value in ["", "#", "fff", "#gg0000", "#12345", "#1234567"] {
            assert_eq!(parse_color(value), None, "`{value}` 本该被拒");
        }
    }

    // ── 应用 ──

    #[test]
    fn layout_attributes_land_on_the_layout_style() {
        let (layout, _) = apply_one("direction", "row");
        assert_eq!(layout.direction, Direction::Row);

        let (layout, _) = apply_one("padding", "12 8");
        assert_eq!(layout.padding.left, 8.0);

        let (layout, _) = apply_one("width", "50%");
        assert_eq!(layout.width, Length::Percent(0.5));
    }

    #[test]
    fn visual_attributes_land_on_the_visual_style() {
        // 分开是因为生命周期不同：布局一变要重排整棵树，
        // 外观只影响这一帧怎么画。
        let (layout, visual) = apply_one("background", "#123");
        assert!(visual.background.is_some());
        assert_eq!(layout, Style::default(), "外观属性不该动布局");
    }

    #[test]
    fn font_size_accepts_both_spellings() {
        assert_eq!(apply_one("size", "18").1.font_size, Some(18.0));
        assert_eq!(apply_one("font_size", "18").1.font_size, Some(18.0));
    }

    #[test]
    fn unknown_attributes_are_not_errors() {
        // 上层可能认识它们：`id`、`on_press`、`name` 都不是样式。
        let mut layout = Style::default();
        let mut visual = Visual::default();
        for name in ["id", "on_press", "name", "完全没见过的"] {
            assert_eq!(
                apply(name, "x", &mut layout, &mut visual),
                Ok(Applied::NotStyle),
                "`{name}` 不该被当成错误"
            );
        }
    }

    #[test]
    fn a_bad_value_names_the_attribute_and_the_value() {
        // 模板是外部文件，报错不指名道姓的话根本找不到。
        let mut layout = Style::default();
        let mut visual = Visual::default();
        let error = apply("padding", "很多", &mut layout, &mut visual).unwrap_err();

        assert_eq!(error.attribute, "padding");
        assert_eq!(error.value, "很多");
        let text = error.to_string();
        assert!(text.contains("padding") && text.contains("很多"), "{text}");
    }

    #[test]
    fn a_bad_value_leaves_the_style_untouched() {
        // 半应用的样式比不应用更难查。
        let mut layout = Style::default();
        let mut visual = Visual::default();
        let _ = apply("direction", "斜的", &mut layout, &mut visual);
        assert_eq!(layout, Style::default());
    }

    // ── 状态前缀 ──

    #[test]
    fn prefixes_are_split_off() {
        assert_eq!(
            Selector::split("background"),
            (Selector::Base, "background")
        );
        assert_eq!(
            Selector::split("hover:background"),
            (Selector::Hover, "background")
        );
        assert_eq!(
            Selector::split("pressed:background"),
            (Selector::Pressed, "background")
        );
    }

    #[test]
    fn an_unknown_prefix_stays_part_of_the_name() {
        // 在这里报「未知前缀」的话，`foo:bar` 会得到两条不同的错误信息，
        // 而它们说的是同一件事。交给上层统一报「未知属性」。
        assert_eq!(Selector::split("foo:bar"), (Selector::Base, "foo:bar"));
    }

    // ── 状态叠加 ──

    #[test]
    fn overlay_only_replaces_what_was_written() {
        // `hover:background` 不该把 `color` 一起清掉。
        let mut base = Visual {
            background: parse_color("#111"),
            color: parse_color("#eee"),
            font_size: Some(14.0),
            ..Default::default()
        };
        let hover = Visual {
            background: parse_color("#333"),
            ..Default::default()
        };
        base.overlay(&hover);

        assert_eq!(base.background, parse_color("#333"));
        assert_eq!(base.color, parse_color("#eee"), "没写的项被盖掉了");
        assert_eq!(base.font_size, Some(14.0));
    }

    #[test]
    fn an_empty_overlay_changes_nothing() {
        let mut base = Visual {
            background: parse_color("#111"),
            ..Default::default()
        };
        let before = base;
        base.overlay(&Visual::default());
        assert_eq!(base, before);
    }

    #[test]
    fn unset_visuals_stay_unset() {
        // 每一项都是 Option：没写的属性该继承或用控件默认值，
        // 而不是被一个「零值」盖掉。模板里不写 color 就等于
        // 把文字设成纯黑的话，所有文字都会看不见。
        let visual = Visual::default();
        assert_eq!(visual.color, None);
        assert_eq!(visual.background, None);
    }
}
