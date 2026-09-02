//! 界面里的一张贴图：图标、头像、物品格子里的东西。
//!
//! # 尺寸要调用方给
//!
//! 控件层不认识 [`ktexture::Texture`]——它只拿到一个 id 和一块 UV。
//! 「这张图原本多大」得由调用方说，因为**图集里的一块的尺寸和整张纹理
//! 的尺寸不是一回事**：一个 32×32 的图标待在 1024² 的图集里，
//! 按纹理尺寸排版会得到一个铺满屏幕的方块。
//!
//! # 贴图会打断合批
//!
//! 整个界面通常只要一次绘制——纯色和文字共用字形图集那一张纹理。
//! 一张外来的贴图**换纹理就得换绑定组**，于是把这一批切成两半。
//!
//! 所以图标该自己先打成一张图集（`ksprite::pack`），全部走同一个
//! 纹理 id、各取各的 UV，一次画完。一张一张贴的话，二十个图标就是
//! 二十次绘制调用。

use kcore::uuid::Uuid;
use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi};

impl WidgetUi {
    /// 一张贴图，按 `size` 占位。
    ///
    /// `texture` 是 [`ktexture::Texture::id`]，而且必须**先登记**过
    /// （`Renderer::register_ui_texture`）——没登记过的批次会被渲染器
    /// 跳过，画面上什么都不出现，也不报错。
    ///
    /// ```no_run
    /// # use kui_widgets::WidgetUi;
    /// # use kmath::Vec2;
    /// # let mut w = WidgetUi::default();
    /// # let atlas_id = kcore::uuid::Uuid::nil();
    /// // 图集里那一格的 UV，自己算或者问 `ksprite::Atlas`。
    /// w.image("coin", atlas_id, Vec2::splat(32.0), [[0.0, 0.0], [0.25, 0.25]]);
    /// ```
    ///
    /// [`ktexture::Texture::id`]: ktexture::Texture::id
    pub fn image(&mut self, id: &str, texture: Uuid, size: Vec2, uv: [[f32; 2]; 2]) -> Id {
        self.image_tinted(id, texture, size, uv, Vec4::ONE)
    }

    /// 一张带染色的贴图。`tint` 会**乘**上去，全白表示原色。
    ///
    /// 染色是灰度图标最省事的用法：一张白色的图标，按状态染成
    /// 启用 / 禁用 / 高亮三种颜色，而不是准备三张图。
    pub fn image_tinted(
        &mut self,
        id: &str,
        texture: Uuid,
        size: Vec2,
        uv: [[f32; 2]; 2],
        tint: Vec4,
    ) -> Id {
        self.push(
            id,
            Widget::Image {
                texture,
                size,
                uv,
                tint,
            },
        )
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
///
/// 就是调用方给的那个尺寸——贴图没有「内在」尺寸可言，理由见模块文档。
pub(crate) fn size(_ui: &Ui, _theme: &Theme, size: Vec2) -> Vec2 {
    size
}

/// 出几何。
///
/// 按**保持长宽比**贴：布局给的矩形不一定是调用方要的那个比例
/// （被 `grow` 拉伸过、被 `shrink` 压过），直接铺满会把图标拉扁。
//
// 八个参数是绘制这一层的统一形状（ui / theme / rect / response 四个固定，
// 后面跟这个控件自己的数据），凑成结构体只会多一个没人复用的类型。
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    ui: &mut Ui,
    _theme: &Theme,
    rect: Rect,
    _response: &Response,
    texture: Uuid,
    size: Vec2,
    uv: [[f32; 2]; 2],
    tint: Vec4,
) {
    let aspect = if size.y > 0.0 { size.x / size.y } else { 0.0 };
    ui.image_fit(rect, aspect, texture, uv, tint);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ui;
    use kui::UiInput;

    fn draw(size: Vec2) -> (kui::Ui, WidgetUi, Id) {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let id = w.image("icon", Uuid::from_u128(7), size, [[0.0, 0.0], [1.0, 1.0]]);
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();
        (ui, w, id)
    }

    #[test]
    fn an_image_reserves_its_declared_size() {
        // 不占位的话它会被压成 0，图画出来也看不见。
        let (_, w, id) = draw(Vec2::new(48.0, 24.0));
        let rect = w.response(id).rect;

        assert!(rect.size().x >= 48.0, "宽度没占住：{:?}", rect.size());
        assert!(rect.size().y >= 24.0, "高度没占住：{:?}", rect.size());
    }

    #[test]
    fn an_image_is_drawn_with_its_own_texture() {
        // 批次的纹理是 `None` 的话画出来的会是字形图集里的一块——
        // 界面上印出一片乱码般的字形，而且不报任何错。
        let (ui, _, _) = draw(Vec2::splat(32.0));

        assert!(!ui.draw_list().is_empty());
        assert!(
            ui.draw_list()
                .batches()
                .iter()
                .any(|batch| batch.texture == Some(Uuid::from_u128(7))),
            "没有一批用的是这张贴图"
        );
    }

    #[test]
    fn an_image_breaks_the_batch_it_lands_in() {
        // 换纹理就得换绑定组，所以贴图前后的纯色图元会被切成两批。
        // 这不是 bug，是代价——图标该先打成一张图集，见模块文档。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.panel("bg");
        w.image(
            "icon",
            Uuid::from_u128(7),
            Vec2::splat(32.0),
            [[0.0, 0.0], [1.0, 1.0]],
        );
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        assert!(
            ui.draw_list().batches().len() >= 2,
            "面板和贴图该分成两批，实际 {} 批",
            ui.draw_list().batches().len()
        );
    }

    #[test]
    fn a_zero_height_image_does_not_divide_by_zero() {
        // 尺寸从别处算出来时可能是 0；NaN 的矩形会让整块界面消失。
        let (ui, _, _) = draw(Vec2::new(32.0, 0.0));
        assert!(
            ui.draw_list()
                .vertices()
                .iter()
                .all(|v| v.position.iter().all(|c| c.is_finite())),
            "出现了 NaN 顶点"
        );
    }

    #[test]
    fn an_image_is_not_focusable() {
        // 图标不是控件，Tab 走上去按回车什么也不会发生。
        let (_, w, id) = draw(Vec2::splat(16.0));
        assert!(!w.response(id).focused);
    }
}
