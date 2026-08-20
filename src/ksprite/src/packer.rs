//! 图集打包：把一堆零散的贴图拼进一张大图。
//!
//! # 为什么需要
//!
//! [`Atlas::grid`](crate::Atlas::grid) 只能处理**等大**的格子。真实的美术
//! 资源是一堆尺寸各异的 PNG——手写格子坐标既痛苦又容易错，改一张图
//! 后面所有坐标都得重算。
//!
//! # 换纹理就是断批
//!
//! 打包的真正意义不是省显存，是**省绘制调用**：一百个精灵用一百张贴图
//! 要提交一百次，拼进一张图集就只要一次。这也是为什么打包器要尽量把
//! 东西塞进**同一张**图集，而不是随便开新的。
//!
//! # 货架装箱
//!
//! 和字形图集同一套办法（见 `kfont::atlas`）：按高度降序排，逐条货架
//! 往里塞。对精灵尤其合适——同一套素材的高度往往接近，浪费很小。
//!
//! 排序是关键：不排序的话，一个高的图会先开一条高货架，后面所有矮图
//! 都挤进去，每个白扔一大截。

use crate::{Atlas, SpriteRegion};
use kmath::Vec2;
use ktexture::Texture;

/// 一张待打包的贴图。
#[derive(Debug, Clone)]
pub struct Entry {
    /// 名字，打包后用它取区域。
    pub name: String,
    /// 贴图本身。
    pub texture: Texture,
}

impl Entry {
    /// 由名字和贴图构造。
    pub fn new(name: impl Into<String>, texture: Texture) -> Self {
        Self {
            name: name.into(),
            texture,
        }
    }
}

/// 打包结果。
#[derive(Debug, Clone)]
pub struct Packed {
    /// 拼好的大图。
    pub texture: Texture,
    /// 各贴图在大图里的区域，按名字索引。
    pub atlas: Atlas,
    /// 没能塞进去的贴图名。
    ///
    /// **不是错误**：图集开小了、或者某张图本身比图集还大，都会走到这里。
    /// 调用方决定是加大图集重来，还是就这样画个占位。
    pub rejected: Vec<String>,
}

/// 打包参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// 图集边长。
    pub size: u32,
    /// 每张图周围留几个像素的空白。
    ///
    /// 不留的话，线性过滤会把邻居的边缘采进来——精灵边上带一道别的图
    /// 的残影，缩小时尤其明显。这个现象叫「渗色」（bleeding）。
    pub padding: u32,
    /// 是否把每张图的边缘像素往外扩一圈填进 padding。
    ///
    /// 光留空白还不够：精灵**自己**的边缘在放大时会和透明的 padding
    /// 混出一圈半透明的边。把边缘复制出去，混出来的还是它自己的颜色。
    pub extrude: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            size: 1024,
            padding: 2,
            extrude: true,
        }
    }
}

/// 一层货架。
struct Shelf {
    y: u32,
    height: u32,
    used: u32,
}

/// 把一组贴图打包成一张图集。
///
/// 塞不下的会出现在 [`Packed::rejected`] 里，而不是让整次打包失败——
/// 少一张图还能跑，整个资源加载失败就跑不起来了。
pub fn pack(entries: &[Entry], options: Options) -> Packed {
    let size = options.size.max(1);
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let mut atlas = Atlas::new();
    let mut rejected = Vec::new();
    let mut shelves: Vec<Shelf> = Vec::new();

    // 按高度降序。不排的话一个高图先开一条高货架，后面所有矮图挤进去，
    // 每个白扔一大截。
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|a, b| {
        entries[*b]
            .texture
            .height()
            .cmp(&entries[*a].texture.height())
            // 高度相同时按名字排，保证同一批输入永远得到同一张图集——
            // 不稳定的话每次构建出来的图集都不一样，没法做增量。
            .then_with(|| entries[*a].name.cmp(&entries[*b].name))
    });

    for index in order {
        let entry = &entries[index];
        let (w, h) = (entry.texture.width(), entry.texture.height());
        let pad = options.padding;
        let (need_w, need_h) = (w + pad * 2, h + pad * 2);

        if w == 0 || h == 0 || need_w > size || need_h > size {
            rejected.push(entry.name.clone());
            continue;
        }

        let Some((x, y)) = allocate(&mut shelves, size, need_w, need_h) else {
            rejected.push(entry.name.clone());
            continue;
        };
        let (x, y) = (x + pad, y + pad);

        blit(&mut pixels, size, entry.texture.data(), w, h, x, y);
        if options.extrude && pad > 0 {
            extrude(&mut pixels, size, w, h, x, y, pad);
        }

        // 区域取的是**不含 padding** 的那一块。含进去的话每个精灵
        // 边上会多出一圈透明像素。
        atlas.push(
            entry.name.clone(),
            SpriteRegion::from_pixels(
                x as f32,
                y as f32,
                w as f32,
                h as f32,
                Vec2::splat(size as f32),
            ),
        );
    }

    Packed {
        texture: Texture::new(size, size, pixels),
        atlas,
        rejected,
    }
}

/// 在货架上找一块地方。
fn allocate(shelves: &mut Vec<Shelf>, size: u32, w: u32, h: u32) -> Option<(u32, u32)> {
    // 找高度够、剩余宽度够、而且浪费不太多的那条。
    //
    // 浪费上限是必要的：没有它的话，一条 200 像素高的货架会把后面所有
    // 16 像素的小图吸进去，每个白扔 184 像素的高度。
    let mut best: Option<(usize, u32)> = None;
    for (index, shelf) in shelves.iter().enumerate() {
        if shelf.height < h || shelf.used + w > size {
            continue;
        }
        let waste = shelf.height - h;
        if waste > shelf.height / 2 {
            continue;
        }
        if best.is_none_or(|(_, best_waste)| waste < best_waste) {
            best = Some((index, waste));
        }
    }
    if let Some((index, _)) = best {
        let shelf = &mut shelves[index];
        let position = (shelf.used, shelf.y);
        shelf.used += w;
        return Some(position);
    }

    let top = shelves.last().map_or(0, |s| s.y + s.height);
    if top + h > size {
        return None;
    }
    shelves.push(Shelf {
        y: top,
        height: h,
        used: w,
    });
    Some((0, top))
}

/// 把一张 RGBA 贴图拷进大图。
fn blit(dst: &mut [u8], dst_size: u32, src: &[u8], w: u32, h: u32, x: u32, y: u32) {
    for row in 0..h {
        let from = (row * w * 4) as usize;
        let to = ((y + row) * dst_size + x) as usize * 4;
        dst[to..to + (w * 4) as usize].copy_from_slice(&src[from..from + (w * 4) as usize]);
    }
}

/// 把贴图的边缘像素往外扩 `pad` 圈。
///
/// 光留透明的 padding 不够：精灵自己的边缘在放大时会和透明像素混出
/// 一圈半透明的边。扩边之后混出来的还是它自己的颜色。
fn extrude(dst: &mut [u8], dst_size: u32, w: u32, h: u32, x: u32, y: u32, pad: u32) {
    let get = |dst: &[u8], px: u32, py: u32| -> [u8; 4] {
        let index = (py * dst_size + px) as usize * 4;
        [dst[index], dst[index + 1], dst[index + 2], dst[index + 3]]
    };
    let put = |dst: &mut [u8], px: u32, py: u32, color: [u8; 4]| {
        if px >= dst_size || py >= dst_size {
            return;
        }
        let index = (py * dst_size + px) as usize * 4;
        dst[index..index + 4].copy_from_slice(&color);
    };

    for ring in 1..=pad {
        // 左右两条边。
        for row in 0..h {
            let left = get(dst, x, y + row);
            let right = get(dst, x + w - 1, y + row);
            if x >= ring {
                put(dst, x - ring, y + row, left);
            }
            put(dst, x + w - 1 + ring, y + row, right);
        }
        // 上下两条边（含刚扩出来的角，所以横向要多走 pad 格）。
        for col in 0..w + pad * 2 {
            let px = (x + col).saturating_sub(pad);
            let top = if y >= ring { get(dst, px, y) } else { [0; 4] };
            let bottom = get(dst, px, y + h - 1);
            if y >= ring {
                put(dst, px, y - ring, top);
            }
            put(dst, px, y + h - 1 + ring, bottom);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张纯色贴图。
    fn solid(w: u32, h: u32, color: [u8; 4]) -> Texture {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            data.extend_from_slice(&color);
        }
        Texture::new(w, h, data)
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new("red", solid(16, 16, [255, 0, 0, 255])),
            Entry::new("green", solid(32, 8, [0, 255, 0, 255])),
            Entry::new("blue", solid(8, 40, [0, 0, 255, 255])),
        ]
    }

    fn pixel(texture: &Texture, x: u32, y: u32) -> [u8; 4] {
        let index = (y * texture.width() + x) as usize * 4;
        let d = texture.data();
        [d[index], d[index + 1], d[index + 2], d[index + 3]]
    }

    #[test]
    fn everything_that_fits_gets_packed() {
        let packed = pack(&entries(), Options::default());
        assert!(packed.rejected.is_empty());
        for name in ["red", "green", "blue"] {
            assert!(packed.atlas.find(name).is_some(), "{name} 没进图集");
        }
    }

    #[test]
    fn regions_do_not_overlap() {
        // 重叠的话两个精灵会互相印在对方身上。
        let packed = pack(&entries(), Options::default());
        let size = packed.texture.width() as f32;

        let rects: Vec<[f32; 4]> = ["red", "green", "blue"]
            .iter()
            .map(|name| {
                let r = packed.atlas.find(name).unwrap();
                [
                    r.min.x * size,
                    r.min.y * size,
                    r.max.x * size,
                    r.max.y * size,
                ]
            })
            .collect();

        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let disjoint = a[2] <= b[0] || b[2] <= a[0] || a[3] <= b[1] || b[3] <= a[1];
                assert!(disjoint, "{a:?} 和 {b:?} 重叠了");
            }
        }
    }

    #[test]
    fn the_region_has_the_right_size() {
        let packed = pack(&entries(), Options::default());
        let size = packed.texture.width() as f32;
        let region = packed.atlas.find("green").unwrap();

        assert!(((region.max.x - region.min.x) * size - 32.0).abs() < 1e-3);
        assert!(((region.max.y - region.min.y) * size - 8.0).abs() < 1e-3);
    }

    #[test]
    fn the_region_excludes_the_padding() {
        // 含进去的话每个精灵边上会多出一圈透明像素。
        let packed = pack(
            &[Entry::new("only", solid(4, 4, [10, 20, 30, 255]))],
            Options {
                size: 64,
                padding: 3,
                extrude: false,
            },
        );
        let region = packed.atlas.find("only").unwrap();
        let x = (region.min.x * 64.0).round() as u32;
        let y = (region.min.y * 64.0).round() as u32;

        assert_eq!(pixel(&packed.texture, x, y), [10, 20, 30, 255]);
        // 区域左上角外面一格该是空的（没开扩边）。
        assert_eq!(pixel(&packed.texture, x - 1, y - 1), [0, 0, 0, 0]);
    }

    #[test]
    fn pixels_land_in_the_right_place() {
        let packed = pack(&entries(), Options::default());
        let size = packed.texture.width() as f32;

        for (name, expected) in [
            ("red", [255, 0, 0, 255]),
            ("green", [0, 255, 0, 255]),
            ("blue", [0, 0, 255, 255]),
        ] {
            let region = packed.atlas.find(name).unwrap();
            let x = (region.min.x * size).round() as u32 + 1;
            let y = (region.min.y * size).round() as u32 + 1;
            assert_eq!(pixel(&packed.texture, x, y), expected, "{name} 的像素不对");
        }
    }

    #[test]
    fn padding_separates_neighbours() {
        // 不留空白的话，线性过滤会把邻居的边缘采进来——精灵边上带一道
        // 别的图的残影，这叫渗色。
        let packed = pack(
            &[
                Entry::new("a", solid(8, 8, [255, 0, 0, 255])),
                Entry::new("b", solid(8, 8, [0, 255, 0, 255])),
            ],
            Options {
                size: 64,
                padding: 2,
                extrude: false,
            },
        );
        let size = 64.0;
        let a = packed.atlas.find("a").unwrap();
        let b = packed.atlas.find("b").unwrap();

        let gap = (b.min.x - a.max.x).abs() * size;
        assert!(gap >= 4.0 - 1e-3, "两张图之间只隔了 {gap} 像素");
    }

    #[test]
    fn extrude_fills_the_padding_with_edge_colour() {
        // 光留透明 padding 不够：精灵自己的边缘放大时会和透明像素混出
        // 一圈半透明的边。
        let packed = pack(
            &[Entry::new("solid", solid(8, 8, [200, 100, 50, 255]))],
            Options {
                size: 32,
                padding: 2,
                extrude: true,
            },
        );
        let region = packed.atlas.find("solid").unwrap();
        let x = (region.min.x * 32.0).round() as u32;
        let y = (region.min.y * 32.0).round() as u32;

        // 左边、上面各一格都该是同一个颜色，而不是透明。
        assert_eq!(pixel(&packed.texture, x - 1, y), [200, 100, 50, 255]);
        assert_eq!(pixel(&packed.texture, x, y - 1), [200, 100, 50, 255]);
        // 角上也要填，否则放大时四个角会漏。
        assert_eq!(pixel(&packed.texture, x - 1, y - 1), [200, 100, 50, 255]);
    }

    #[test]
    fn an_oversized_texture_is_rejected_not_fatal() {
        // 少一张图还能跑，整个资源加载失败就跑不起来了。
        let packed = pack(
            &[
                Entry::new("huge", solid(200, 200, [1, 2, 3, 4])),
                Entry::new("small", solid(8, 8, [9, 9, 9, 255])),
            ],
            Options {
                size: 64,
                padding: 1,
                extrude: false,
            },
        );
        assert_eq!(packed.rejected, vec!["huge".to_string()]);
        assert!(packed.atlas.find("small").is_some(), "小图该照常打包");
    }

    #[test]
    fn a_zero_sized_texture_is_rejected() {
        let packed = pack(
            &[Entry::new("empty", Texture::new(0, 0, Vec::new()))],
            Options::default(),
        );
        assert_eq!(packed.rejected.len(), 1);
    }

    #[test]
    fn packing_is_deterministic() {
        // 不稳定的话每次构建出来的图集都不一样，没法做增量。
        let a = pack(&entries(), Options::default());
        let b = pack(&entries(), Options::default());
        for name in ["red", "green", "blue"] {
            assert_eq!(a.atlas.find(name), b.atlas.find(name));
        }
        assert_eq!(a.texture.data(), b.texture.data());
    }

    #[test]
    fn tall_textures_are_placed_first() {
        // 不按高度排序的话，一个高图先开一条高货架，后面所有矮图
        // 都挤进去，每个白扔一大截。
        let packed = pack(&entries(), Options::default());
        let blue = packed.atlas.find("blue").unwrap(); // 8×40，最高
        // 区域不含 padding，所以第一条货架上的图 y 正好等于 padding。
        let y_px = blue.min.y * packed.texture.width() as f32;
        assert!(
            y_px <= Options::default().padding as f32 + 1e-3,
            "最高的那张该排在第一条货架上，实测 y = {y_px} 像素"
        );
    }

    #[test]
    fn short_textures_do_not_waste_a_tall_shelf() {
        // 没有浪费上限的话，一条 200 像素高的货架会把后面所有 16 像素的
        // 小图吸进去，每个白扔 184 像素。
        let entries = vec![
            Entry::new("tall", solid(8, 200, [1, 1, 1, 255])),
            Entry::new("short_a", solid(8, 8, [2, 2, 2, 255])),
            Entry::new("short_b", solid(8, 8, [3, 3, 3, 255])),
        ];
        let packed = pack(
            &entries,
            Options {
                size: 256,
                padding: 1,
                extrude: false,
            },
        );
        let tall = packed.atlas.find("tall").unwrap();
        let short = packed.atlas.find("short_a").unwrap();
        assert!(
            short.min.y > tall.min.y + 0.1,
            "矮图挤进了高货架：矮图 y = {}，高图 y = {}",
            short.min.y,
            tall.min.y
        );
    }

    #[test]
    fn everything_stays_inside_the_atlas() {
        let entries: Vec<Entry> = (0..40)
            .map(|i| Entry::new(format!("s{i}"), solid(5 + i % 11, 7 + i % 13, [i as u8; 4])))
            .collect();
        let packed = pack(
            &entries,
            Options {
                size: 128,
                padding: 1,
                extrude: true,
            },
        );

        for name in entries.iter().map(|e| &e.name) {
            let Some(region) = packed.atlas.find(name) else {
                continue;
            };
            assert!(region.min.x >= 0.0 && region.min.y >= 0.0);
            assert!(region.max.x <= 1.0 + 1e-6 && region.max.y <= 1.0 + 1e-6);
        }
    }

    #[test]
    fn an_empty_input_produces_an_empty_atlas() {
        let packed = pack(&[], Options::default());
        assert!(packed.rejected.is_empty());
        assert_eq!(packed.texture.width(), 1024);
    }
}
