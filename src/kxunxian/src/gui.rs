//! 寻仙的界面资源（`gui/`）：界面贴图、玩家头像、技能图标。
//!
//! 原版界面是一套自己的 XML 布局（`gui/group/**/*.xml`）+ PNG 贴图。贴图多是图集：
//! 布局里写 `image="/image/main_bar/pic_card_group.png|43,0,42,42"`，竖线后面是
//! `x,y,宽,高`（超出贴图的部分按贴图边截掉——`pic_deck_l.png|44,0,128,82` 实际只取到 84 宽），
//! 按钮的几种状态竖着排在一张图里（`vertical_image="true"`）。这里只管把贴图读出来，
//! 怎么拼由调用方照着布局 XML 摆。
//!
//! # 表格是 GBK 的
//!
//! `mb/` 下的表格（`cha_pic.txt`、`skill_data_*.txt`）是 GBK 编码的制表符分隔文本。
//! 这里只取 ASCII 的列（`.cct` 路径、`zj07.png`、图标号），GBK 的多字节字符里不会出现
//! 制表符和换行，按字节切行切列就够了，不用解码。

use std::collections::HashMap;

use ktexture::{FilterMode, Sampler, Texture, TextureFormat, WrapMode};

use crate::character::Library;

/// 界面贴图的采样：线性、贴边、不要 mip（界面按原尺寸画，mip 只会让缩小时发糊、还多占显存）。
fn ui_sampler() -> Sampler {
    Sampler {
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        wrap_u: WrapMode::ClampToEdge,
        wrap_v: WrapMode::ClampToEdge,
        mipmaps: false,
        anisotropy: 1,
    }
}

/// 一张表按字节切成行、每行按制表符切成列（列是原始字节，GBK 的不解码）。
fn table_rows(bytes: &[u8]) -> impl Iterator<Item = Vec<&[u8]>> {
    bytes
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .map(|line| line.split(|&b| b == b'\t').collect())
}

/// `cha_pic.txt` 里某个身体的头像文件名（`zj07.png`）。第一条用到这个身体 `.cct` 的就是玩家角色本身
/// （后面那些是穿着不同衣服的 NPC 版本，头像一样）。
pub fn head_icon_of(cha_pic: &[u8], body: &str) -> Option<String> {
    let needle = format!("special\\{body}\\config\\{body}.cct").to_ascii_lowercase();
    table_rows(cha_pic).find_map(|columns| {
        let uses_body = columns
            .iter()
            .any(|c| c.to_ascii_lowercase().as_slice() == needle.as_bytes());
        if !uses_body {
            return None;
        }
        columns.iter().find_map(|c| {
            let text = std::str::from_utf8(c).ok()?;
            let stem = text.strip_suffix(".png")?;
            (stem.starts_with("zj") && stem[2..].chars().all(|ch| ch.is_ascii_digit()))
                .then(|| text.to_string())
        })
    })
}

/// `skill_data_*.txt` 里每个技能的 `(编号, 图标号)`，按表里的顺序。第 2 列是编号，第 4 列是图标号
/// （`icon/skill/<图标号>.png`）；没有图标的行跳过。
pub fn skill_icons(table: &[u8]) -> Vec<(u32, u32)> {
    table_rows(table)
        .filter_map(|columns| {
            let number = |index: usize| -> Option<u32> {
                std::str::from_utf8(columns.get(index)?)
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            Some((number(1)?, number(3)?))
        })
        .collect()
}

impl Library {
    /// 读一张界面贴图，路径照布局 XML 里写的（`/image/main_bar/pic_deck_l.png`，相对 `gui/`）。
    /// 读不到返回 `None`。
    pub fn gui_image(&self, path: &str) -> Option<Texture> {
        let path = path.split('|').next()?.trim_start_matches(['/', '\\']);
        let (_, bytes) = self.read(&format!("gui/{path}")).ok()?;
        let texture = Texture::from_encoded(&bytes).ok()?;
        Some(
            texture
                .with_format(TextureFormat::Srgb)
                .with_sampler(ui_sampler()),
        )
    }

    /// 玩家角色（`zj_shusheng_007` 这类身体）的头像，已按原版的 `circle_mask.png` 抠成圆的
    /// （原版界面是画的时候叠遮罩，这里烘进 alpha，画起来就是一张普通贴图）。
    ///
    /// `mood` 是表情 0～3（原版按心情换头像，`zj07_<心情>.png`）。查不到这个身体就用默认头像。
    pub fn portrait(&self, body: &str, mood: u32) -> Option<Texture> {
        let file = self
            .read("mb/object/cha_pic.txt")
            .ok()
            .and_then(|(_, table)| head_icon_of(&table, body))
            .map(|icon| {
                format!(
                    "/icon/portrait/{}_{mood}.png",
                    icon.trim_end_matches(".png")
                )
            });
        let portrait = file
            .and_then(|file| self.gui_image(&file))
            .or_else(|| self.gui_image("/icon/portrait/default.png"))?;
        let Some(mask) = self.gui_image("/image/frame/circle_mask.png") else {
            return Some(portrait);
        };
        Some(apply_mask(&portrait, &mask))
    }

    /// 一个门派技能表（`mb/skill/skill_data_<门派>.txt`，比如 `fs`）里前 `count` 个有图标的技能：
    /// `(技能编号, 图标贴图)`。
    pub fn skill_icon_textures(&self, school: &str, count: usize) -> Vec<(u32, Texture)> {
        let Ok((_, table)) = self.read(&format!("mb/skill/skill_data_{school}.txt")) else {
            return Vec::new();
        };
        let mut seen = HashMap::new();
        skill_icons(&table)
            .into_iter()
            .filter(|&(_, icon)| seen.insert(icon, ()).is_none())
            .filter_map(|(id, icon)| {
                Some((id, self.gui_image(&format!("/icon/skill/{icon}.png"))?))
            })
            .take(count)
            .collect()
    }
}

/// 一个场景的小地图图块（`gui/image/map/mini_map/<场景>/<场景>_<x>_<z>.png`），一个地形单元一块。
///
/// 图块就是那个单元的预混贴图转了 180°（拿河道、村子的位置核对过）：像素的列是本地 x、
/// **行是 255 − 本地 z**，所以图上往右是 +x、往上是原始数据的 +z（引擎的 −z）。
pub struct MiniMapTiles {
    tiles: HashMap<(i32, i32), Texture>,
    arrow: Option<Texture>,
}

impl Library {
    /// 读一个场景的小地图图块，`units` 是要读的单元坐标。
    pub fn minimap_tiles(
        &self,
        scene: &str,
        units: impl IntoIterator<Item = (i32, i32)>,
    ) -> MiniMapTiles {
        let tiles = units
            .into_iter()
            .filter_map(|(x, z)| {
                let path = format!("/image/map/mini_map/{scene}/{scene}_{x}_{z}.png");
                Some(((x, z), self.gui_image(&path)?))
            })
            .collect();
        MiniMapTiles {
            tiles,
            arrow: self.gui_image("/image/map/player_arrow.png"),
        }
    }
}

impl MiniMapTiles {
    /// 一块图都没读到。
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// 以 `center`（引擎坐标）为中心、每像素 `meters_per_pixel` 米，画一张 `size` × `size` 的圆形小地图
    /// （RGBA，圆外透明，边缘一像素的过渡），正中画玩家箭头，朝 `heading`（引擎里的朝向，弧度，0 是 +z）。
    /// 图上方是原始数据的 +z（引擎的 −z）。没有图块的地方画成暗色。
    pub fn render(
        &self,
        center: kmath::Vec3,
        meters_per_pixel: f32,
        heading: f32,
        size: usize,
    ) -> Vec<u8> {
        let mut out = vec![0u8; size * size * 4];
        let half = size as f32 * 0.5;
        let (raw_x, raw_z) = (center.x, -center.z);
        for j in 0..size {
            for i in 0..size {
                let (dx, dy) = (i as f32 + 0.5 - half, half - (j as f32 + 0.5));
                let edge = half - (dx * dx + dy * dy).sqrt();
                if edge <= 0.0 {
                    continue;
                }
                let x = raw_x + dx * meters_per_pixel;
                let z = raw_z + dy * meters_per_pixel;
                let (ux, uz) = ((x / 256.0).floor() as i32, (z / 256.0).floor() as i32);
                let rgb = match self.tiles.get(&(ux, uz)) {
                    Some(tile) => {
                        let (w, h) = (tile.width() as usize, tile.height() as usize);
                        let lx = x - ux as f32 * 256.0;
                        let lz = z - uz as f32 * 256.0;
                        let col = ((lx / 256.0 * w as f32) as usize).min(w - 1);
                        let row = (((256.0 - lz) / 256.0 * h as f32) as usize).min(h - 1);
                        let at = (row * w + col) * 4;
                        [tile.data()[at], tile.data()[at + 1], tile.data()[at + 2]]
                    }
                    None => [24, 30, 36],
                };
                let at = (j * size + i) * 4;
                out[at..at + 3].copy_from_slice(&rgb);
                out[at + 3] = (edge.min(1.0) * 255.0) as u8;
            }
        }
        if let Some(arrow) = &self.arrow {
            draw_rotated(&mut out, size, arrow, std::f32::consts::PI - heading);
        }
        out
    }
}

/// 把 `sprite` 绕自己中心顺时针转 `angle` 弧度，贴到 `size` × `size` 的图正中（按 alpha 混合）。
fn draw_rotated(out: &mut [u8], size: usize, sprite: &Texture, angle: f32) {
    let (w, h) = (sprite.width() as i32, sprite.height() as i32);
    let (sin, cos) = angle.sin_cos();
    let reach = (w.max(h) as f32 * 0.75) as i32;
    let center = size as i32 / 2;
    for y in -reach..reach {
        for x in -reach..reach {
            // 屏幕上的偏移（y 朝下）反转回贴图里的位置。
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let sx = px * cos + py * sin + w as f32 * 0.5;
            let sy = -px * sin + py * cos + h as f32 * 0.5;
            if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
                continue;
            }
            let (ox, oy) = (center + x, center + y);
            if ox < 0 || oy < 0 || ox >= size as i32 || oy >= size as i32 {
                continue;
            }
            let src = &sprite.data()[((sy as i32 * w + sx as i32) * 4) as usize..][..4];
            let a = src[3] as u32;
            if a == 0 {
                continue;
            }
            let at = (oy as usize * size + ox as usize) * 4;
            for c in 0..3 {
                out[at + c] = ((src[c] as u32 * a + out[at + c] as u32 * (255 - a)) / 255) as u8;
            }
            out[at + 3] = out[at + 3].max(src[3]);
        }
    }
}

/// 把 `mask` 的 alpha（按比例采样到同样大小）乘进 `image` 的 alpha。
fn apply_mask(image: &Texture, mask: &Texture) -> Texture {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let (mw, mh) = (mask.width() as usize, mask.height() as usize);
    let mut pixels = image.data().to_vec();
    for y in 0..h {
        for x in 0..w {
            let m = mask.data()[((y * mh / h) * mw + x * mw / w) * 4 + 3] as u32;
            let at = (y * w + x) * 4 + 3;
            pixels[at] = (pixels[at] as u32 * m / 255) as u8;
        }
    }
    Texture::new(w as u32, h as u32, pixels)
        .with_format(TextureFormat::Srgb)
        .with_sampler(ui_sampler())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_head_icon_of_a_body_without_decoding_gbk() {
        // 第一列是 GBK 的中文名（这里随便放几个高位字节），只认 ASCII 的列。
        let mut table = Vec::new();
        table.extend_from_slice(b"\xd6\xf7\xbd\xc7_zj_laotou_001\t9\tspecial\\zj_laotou_001\\config\\zj_laotou_001.cct\t\t1.25\tzj01.png\t30\r\n");
        table.extend_from_slice(b"\xd6\xf7\xbd\xc7_zj_shusheng_007\t21\tspecial\\zj_shusheng_007\\config\\zj_shusheng_007.cct\t\tzj07.png\r\n");
        assert_eq!(
            head_icon_of(&table, "zj_shusheng_007").as_deref(),
            Some("zj07.png")
        );
        assert_eq!(
            head_icon_of(&table, "zj_laotou_001").as_deref(),
            Some("zj01.png")
        );
        assert_eq!(head_icon_of(&table, "zj_nobody_000"), None);
    }

    #[test]
    fn reads_skill_numbers_and_icons() {
        let table = b"\r\n\xc9\xc1\xb5\xe7\t4001\t40010*40057\t217\t\xb4\xd3\r\n\xc0\xd7\t4002\t40060\t218\t\r\nbad\tx\t\t\r\n";
        assert_eq!(skill_icons(table), vec![(4001, 217), (4002, 218)]);
    }

    #[test]
    fn the_minimap_samples_tiles_turned_round_and_is_round() {
        // 单元 (0,0) 的图块：左上角一个红点（列 0、行 0 = 本地 x 0、本地 z 255）。
        let mut pixels = vec![0u8; 256 * 256 * 4];
        pixels[..4].copy_from_slice(&[255, 0, 0, 255]);
        for p in pixels.chunks_exact_mut(4).skip(1) {
            p.copy_from_slice(&[0, 0, 255, 255]);
        }
        let tiles = MiniMapTiles {
            tiles: [((0, 0), Texture::new(256, 256, pixels))]
                .into_iter()
                .collect(),
            arrow: None,
        };
        // 中心对准原始 (0.5, 255.5)，也就是引擎 (0.5, _, -255.5)：正中那个像素该是红的。
        let size = 9;
        let out = tiles.render(kmath::Vec3::new(0.5, 0.0, -255.5), 1.0, 0.0, size);
        let at = |i: usize, j: usize| &out[(j * size + i) * 4..][..4];
        assert_eq!(&at(4, 4)[..3], &[255, 0, 0]);
        // 右边一格（+x）还在单元里，是蓝的；上面一格（原始 +z）出了单元，是暗色。
        assert_eq!(&at(5, 4)[..3], &[0, 0, 255]);
        assert_eq!(&at(4, 3)[..3], &[24, 30, 36]);
        // 圆外透明。
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(4, 4)[3], 255);
    }

    #[test]
    fn a_mask_cuts_the_alpha() {
        let image = Texture::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]);
        let mask = Texture::new(2, 1, vec![0, 0, 0, 0, 0, 0, 0, 255]);
        let out = apply_mask(&image, &mask);
        assert_eq!(out.data()[3], 0);
        assert_eq!(out.data()[7], 255);
    }
}
