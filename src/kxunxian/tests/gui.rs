//! 界面资源（`gui/`）：头像、技能图标、界面贴图。资源不在就跳过。

use std::path::PathBuf;

use kxunxian::Library;

fn library() -> Option<Library> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/unpack2");
    root.join("gui").is_dir().then(|| Library::new(&root))
}

#[test]
fn a_player_portrait_is_round() {
    let Some(library) = library() else { return };
    let portrait = library.portrait("zj_shusheng_007", 0).expect("书生的头像");
    let (w, h) = (portrait.width() as usize, portrait.height() as usize);
    let alpha = |x: usize, y: usize| portrait.data()[(y * w + x) * 4 + 3];
    // 圆形遮罩：四角透明，中间不透明。
    assert_eq!(alpha(0, 0), 0);
    assert_eq!(alpha(w - 1, h - 1), 0);
    assert!(alpha(w / 2, h / 2) > 200);
    // 是书生自己的头像（`zj07_0.png`），不是查不到时退回的默认头像（一张白图）。
    let at = (h / 2 * w + w / 2) * 4;
    let center = &portrait.data()[at..at + 3];
    assert!(
        center.iter().any(|&c| c < 230),
        "头像中心是 {center:?}，像是退回了默认的白图"
    );
}

#[test]
fn skill_icons_and_bar_textures_load() {
    let Some(library) = library() else { return };
    let skills = library.skill_icon_textures("fs", 8);
    assert_eq!(skills.len(), 8, "法师技能表里该有 8 个带图标的技能");
    for path in [
        "/image/main_bar/pic_deck_l.png",
        "/image/main_bar/pic_card_group.png|0,0,42,42",
        "/image/team/team_frame.png",
    ] {
        assert!(library.gui_image(path).is_some(), "{path} 读不到");
    }
}

#[test]
fn the_minimap_of_xiaosangcun_has_a_tile_per_unit() {
    let Some(mut library) = library() else { return };
    let scene = library.scene("fb_xiaosangcun").unwrap();
    let units: Vec<(i32, i32)> = scene.units.iter().map(|u| (u.x, u.z)).collect();
    let tiles = library.minimap_tiles("fb_xiaosangcun", units);
    assert!(!tiles.is_empty());
    // 出生点那一块画出来：圆里不透明、不是没图块时的暗色。
    let spawn = scene.mark("playerbegin").expect("出生点").position;
    let size = 64;
    let image = tiles.render(spawn, 1.0, 0.0, size);
    let at = (size / 2 * size + size / 4) * 4;
    assert_eq!(image[at + 3], 255);
    assert_ne!(&image[at..at + 3], &[24, 30, 36]);
}
