//! 动态字形图集。
//!
//! 把光栅化出来的字形塞进一张灰度图，渲染器只上传这一张，
//! 一整屏文字就能一次绘制画完。
//!
//! # 为什么必须能驱逐
//!
//! 拉丁文常用字形不到两百个，一张 512×512 装得下，塞满就完事。
//! **中文不是**：常用字三千起步，加上不同字号，同一张图集里可能出现几万个
//! 条目。图集满了就不画新字的话，用户会看到一段中文突然缺字。
//!
//! 所以满了要驱逐——按 **LRU**：淘汰最久没被用到的那些。
//! 这条策略贴合实际使用：屏幕上的文字有很强的局部性，
//! 一个界面反复出现的就那几十个字。
//!
//! # 货架装箱
//!
//! 用货架（shelf）而不是通用矩形装箱：字形高度按字号聚集，
//! 同一行里高度接近，浪费很小；而货架的插入是 O(货架数)，
//! 通用装箱是 O(空闲矩形数)，后者在几万次插入下明显更贵。

use fxhash::FxHashMap;
use ktexture::{FilterMode, Sampler, Texture, TextureFormat, WrapMode};

/// 一个字形在图集里的键。
///
/// 字号进了键：同一个字形在 12 px 和 48 px 下是两张不同的位图，
/// 放大小的那张会糊。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// 哪个字体。
    pub font: u32,
    /// 字体内部的字形号（不是字符码位——连字、异体字都对应同一个字符）。
    pub glyph: u16,
    /// 字号，单位 1/64 像素。
    ///
    /// 用定点数而不是 `f32`：浮点不能当哈希键，而且 12.000001 px 和
    /// 12.0 px 不该是两个条目。
    pub size: u32,
}

impl GlyphKey {
    /// 由像素字号构造。字号会被量化到 1/64 像素。
    pub fn new(font: u32, glyph: u16, size_px: f32) -> Self {
        Self {
            font,
            glyph,
            size: (size_px.max(0.0) * 64.0).round() as u32,
        }
    }

    /// 还原成像素字号。
    pub fn size_px(&self) -> f32 {
        self.size as f32 / 64.0
    }
}

/// 一个字形在图集里的位置与度量。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphEntry {
    /// 在图集里的像素矩形：`[x, y, 宽, 高]`。
    pub rect: [u32; 4],
    /// 相对笔位置的左偏移（像素）。
    pub bearing_x: f32,
    /// 相对基线的**上**偏移（像素，向上为正）。
    pub bearing_y: f32,
    /// 画完这个字形笔要前进多少像素。
    pub advance: f32,
    /// 上一次被用到是第几帧。驱逐按这个排。
    last_used: u64,
}

impl GlyphEntry {
    /// 归一化 UV：`[u0, v0, u1, v1]`。
    pub fn uv(&self, atlas_size: u32) -> [f32; 4] {
        let s = atlas_size as f32;
        [
            self.rect[0] as f32 / s,
            self.rect[1] as f32 / s,
            (self.rect[0] + self.rect[2]) as f32 / s,
            (self.rect[1] + self.rect[3]) as f32 / s,
        ]
    }

    /// 字形位图是不是空的（空格之类）。
    pub fn is_blank(&self) -> bool {
        self.rect[2] == 0 || self.rect[3] == 0
    }
}

/// 一个刚光栅化出来的字形：位图加度量。
///
/// 打包成一个结构体而不是七个参数——参数一多，调用点上
/// `bearing_x` 和 `bearing_y` 传反了编译器是不会说话的。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GlyphImage {
    /// 覆盖率位图，`width × height`，行优先。空白字形为空。
    pub coverage: Vec<u8>,
    /// 位图宽。
    pub width: u32,
    /// 位图高。
    pub height: u32,
    /// 相对笔位置的左偏移（像素）。
    pub bearing_x: f32,
    /// 相对基线的**上**偏移（像素，向上为正）。
    pub bearing_y: f32,
    /// 画完之后笔要前进多少像素。
    pub advance: f32,
}

/// 一层货架。
#[derive(Debug, Clone, Copy)]
struct Shelf {
    /// 货架顶边的 y。
    y: u32,
    /// 货架高度。放不进这个高度的字形要开新货架。
    height: u32,
    /// 已经用掉的宽度。
    used: u32,
}

/// 插入失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    /// 字形本身比整张图集还大。驱逐再多次也没用。
    TooLarge,
    /// 图集满了，而且已经无可驱逐。
    Full,
}

/// 动态字形图集。
pub struct GlyphAtlas {
    size: u32,
    /// 单通道覆盖率位图。0 = 全透明，255 = 全不透明。
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    entries: FxHashMap<GlyphKey, GlyphEntry>,
    /// 当前帧号，用来记 LRU。
    frame: u64,
    /// 像素内容的版本号。渲染器靠它判断要不要重新上传。
    version: u64,
    /// 累计驱逐了多少个字形。用来判断图集是不是开小了。
    evictions: u64,
    /// 自上次重排以来被驱逐腾出的像素数。
    ///
    /// 货架是单调向右生长的，驱逐留下的洞不会被复用；洞多到一定程度
    /// 就该整体重排，否则会陷入「一直插入失败、一直驱逐」的空转。
    holes: u64,
}

impl GlyphAtlas {
    /// 字形之间留一像素空隙。
    ///
    /// 不留的话，双线性采样会把邻居的边缘吃进来——表现为字的边上带一道
    /// 别的字的残影，字号越小越明显。
    const PADDING: u32 = 1;

    /// 白色像素在图集左上角占的方块边长。
    ///
    /// 留一块纯白，UI 的纯色矩形就能采样它，于是**纯色和文字共用一张纹理**，
    /// 一整个界面一次绘制就能画完。取 2×2 而不是 1×1：线性过滤会在
    /// 单像素的边上把邻居的 0 采进来，纯色块的边缘会发虚。
    pub const WHITE_TEXEL: u32 = 2;

    /// 建一张 `size × size` 的图集。
    pub fn new(size: u32) -> Self {
        let size = size.max(1);
        let mut atlas = Self {
            size,
            pixels: vec![0; (size * size) as usize],
            shelves: Vec::new(),
            entries: FxHashMap::default(),
            frame: 0,
            version: 0,
            evictions: 0,
            holes: 0,
        };
        atlas.reserve_white_texel();
        atlas
    }

    /// 纯白方块的 UV 中心。纯色绘制采样这一点。
    pub fn white_uv(&self) -> [f32; 2] {
        let half = Self::WHITE_TEXEL as f32 * 0.5 / self.size as f32;
        [half, half]
    }

    /// 在左上角占一块纯白，并开一条对应的货架把它挡住。
    fn reserve_white_texel(&mut self) {
        let n = Self::WHITE_TEXEL.min(self.size);
        for row in 0..n {
            let start = (row * self.size) as usize;
            self.pixels[start..start + n as usize].fill(255);
        }
        // 开一条恰好这么高的货架并把这块宽度占掉，字形就不会覆盖它。
        self.shelves.push(Shelf {
            y: 0,
            height: n,
            used: n,
        });
    }

    /// 边长。
    pub fn size(&self) -> u32 {
        self.size
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 一个字形都没有。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 像素内容的版本号，每次写入递增。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 累计驱逐次数。持续增长说明图集开小了。
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// 开始新一帧。LRU 的时间基准靠它推进。
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// 查一个字形，同时把它标记为「本帧用过」。
    pub fn get(&mut self, key: &GlyphKey) -> Option<GlyphEntry> {
        let frame = self.frame;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = frame;
        Some(*entry)
    }

    /// 查一个字形但**不**更新 LRU。给统计与测试用。
    pub fn peek(&self, key: &GlyphKey) -> Option<GlyphEntry> {
        self.entries.get(key).copied()
    }

    /// 塞一个字形进来。
    ///
    /// `coverage` 是 `width × height` 的单通道位图，行优先。
    /// 宽或高为 0 表示空白字形（空格），仍然会记条目——
    /// 不记的话每帧都会重新光栅化一次空格。
    pub fn insert(&mut self, key: GlyphKey, image: &GlyphImage) -> Result<GlyphEntry, AtlasError> {
        let GlyphImage {
            width,
            height,
            bearing_x,
            bearing_y,
            advance,
            ..
        } = *image;
        let coverage = &image.coverage;

        if width == 0 || height == 0 {
            let entry = GlyphEntry {
                rect: [0, 0, 0, 0],
                bearing_x,
                bearing_y,
                advance,
                last_used: self.frame,
            };
            self.entries.insert(key, entry);
            return Ok(entry);
        }

        if width + Self::PADDING > self.size || height + Self::PADDING > self.size {
            return Err(AtlasError::TooLarge);
        }

        // 装不下就驱逐再试。反复驱逐直到腾出位置或者确认无可驱逐。
        let rect = loop {
            if let Some(rect) = self.allocate(width, height) {
                break rect;
            }
            if !self.evict_one() {
                return Err(AtlasError::Full);
            }
        };

        // 写像素。
        for row in 0..height {
            let src = (row * width) as usize;
            let dst = ((rect[1] + row) * self.size + rect[0]) as usize;
            self.pixels[dst..dst + width as usize]
                .copy_from_slice(&coverage[src..src + width as usize]);
        }
        self.version += 1;

        let entry = GlyphEntry {
            rect,
            bearing_x,
            bearing_y,
            advance,
            last_used: self.frame,
        };
        self.entries.insert(key, entry);
        Ok(entry)
    }

    /// 把图集内容取成一张纹理。
    ///
    /// 展开成 RGB = 白、A = 覆盖率的 RGBA8。多花四倍显存
    /// （1024² 图集是 4 MB），换来的是**不必给渲染器加一条单通道路径**：
    /// 普通的 alpha 混合直接就能用，文字颜色由顶点色乘上去。
    ///
    /// 格式是 **Linear 不是 sRGB**：覆盖率是几何量，不是颜色，
    /// 走 sRGB 解码会让字重整体变细。
    pub fn to_texture(&self) -> Texture {
        let mut rgba = Vec::with_capacity(self.pixels.len() * 4);
        for coverage in &self.pixels {
            rgba.extend_from_slice(&[255, 255, 255, *coverage]);
        }
        Texture::new(self.size, self.size, rgba)
            .with_format(TextureFormat::Linear)
            // 夹取而不是重复：字形贴着图集边缘时，重复采样会把对面边上的
            // 像素卷进来。线性过滤是要的——最近邻会让小字锯齿严重。
            .with_sampler(Sampler {
                min_filter: FilterMode::Linear,
                mag_filter: FilterMode::Linear,
                wrap_u: WrapMode::ClampToEdge,
                wrap_v: WrapMode::ClampToEdge,
            })
    }

    /// 原始像素，行优先单通道。
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// 清空全部条目与像素。换字体、换 DPI 时用。
    pub fn clear(&mut self) {
        self.pixels.fill(0);
        self.shelves.clear();
        self.entries.clear();
        self.holes = 0;
        // 白块要重新占上——清完不补的话，纯色矩形会突然变透明。
        self.reserve_white_texel();
        self.version += 1;
    }

    /// 在货架上找一块地方。找不到返回 `None`。
    fn allocate(&mut self, width: u32, height: u32) -> Option<[u32; 4]> {
        let need_w = width + Self::PADDING;
        let need_h = height + Self::PADDING;

        // 先找现成的货架：高度够、剩余宽度也够、而且**浪费不太多**。
        //
        // 浪费上限是关键。没有它的话，一个 30 px 的货架会把后续所有
        // 6 px 的字形都吸进去，每个白扔 24 px 的高度——一张图集很快
        // 就被这种空隙吃光。宁可另开一条矮货架。
        let mut best: Option<(usize, u32)> = None;
        for (i, shelf) in self.shelves.iter().enumerate() {
            if shelf.height < need_h || shelf.used + need_w > self.size {
                continue;
            }
            let waste = shelf.height - need_h;
            if waste > shelf.height / 2 {
                continue;
            }
            if best.is_none_or(|(_, w)| waste < w) {
                best = Some((i, waste));
            }
        }
        if let Some((i, _)) = best {
            let shelf = &mut self.shelves[i];
            let rect = [shelf.used, shelf.y, width, height];
            shelf.used += need_w;
            return Some(rect);
        }

        // 开新货架。
        let top = self
            .shelves
            .last()
            .map_or(0, |shelf| shelf.y + shelf.height);
        if top + need_h > self.size {
            return None;
        }
        self.shelves.push(Shelf {
            y: top,
            height: need_h,
            used: need_w,
        });
        Some([0, top, width, height])
    }

    /// 淘汰一个最久没用的字形。返回是否真的淘汰了。
    ///
    /// **本帧用过的不淘汰**：正在画的字被淘汰掉，会导致同一帧内反复
    /// 插入-淘汰同一批字形，画面上表现为文字闪烁。
    fn evict_one(&mut self) -> bool {
        let frame = self.frame;
        let victim = self
            .entries
            .iter()
            .filter(|(_, e)| e.last_used < frame && !e.is_blank())
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| *k);

        let Some(victim) = victim else {
            return false;
        };
        let entry = self.entries.remove(&victim).expect("刚查到的条目应当还在");

        // 只清像素，不回收货架空间。
        //
        // 货架是单调向右生长的，中间挖个洞也用不上——要真正复用就得做
        // 空闲块合并，那是通用装箱的复杂度。这里换一种做法：
        // 洞多到装不下新字时，整张图集重建（见 `compact`）。
        for row in 0..entry.rect[3] {
            let dst = ((entry.rect[1] + row) * self.size + entry.rect[0]) as usize;
            self.pixels[dst..dst + entry.rect[2] as usize].fill(0);
        }
        self.version += 1;
        self.evictions += 1;
        self.holes += u64::from(entry.rect[2]) * u64::from(entry.rect[3]);

        // 洞占到图集的四分之一就重排一次。
        //
        // 货架单调向右生长，中间挖的洞用不上；不重排的话会陷入
        // 「插入失败 → 驱逐 → 还是失败」的空转。阈值取四分之一是个折中：
        // 太小会频繁重排，太大则空转期变长。
        if self.holes * 4 >= u64::from(self.size) * u64::from(self.size) {
            self.compact();
        }
        true
    }

    /// 把还活着的条目紧凑地重排一遍，回收所有洞。
    ///
    /// **搬像素，不丢条目。** 早先图省事写成「整张清掉、让调用方重新
    /// 光栅化」，结果是本帧正在用的字形也被一起清了——LRU 保证被自己的
    /// 压实逻辑架空，而且下一帧要重新光栅化几百个字形。
    fn compact(&mut self) {
        // 高的先排。矮的先排会先占满低货架，把高字形挤到没处放。
        let mut alive: Vec<(GlyphKey, GlyphEntry)> =
            self.entries.iter().map(|(k, e)| (*k, *e)).collect();
        alive.sort_unstable_by(|a, b| {
            b.1.rect[3]
                .cmp(&a.1.rect[3])
                .then_with(|| b.1.last_used.cmp(&a.1.last_used))
        });

        let blank = vec![0; self.pixels.len()];
        let old_pixels = std::mem::replace(&mut self.pixels, blank);
        self.shelves.clear();
        self.entries.clear();
        self.holes = 0;
        self.reserve_white_texel();

        for (key, mut entry) in alive {
            if entry.is_blank() {
                self.entries.insert(key, entry);
                continue;
            }
            let (w, h) = (entry.rect[2], entry.rect[3]);
            let Some(rect) = self.allocate(w, h) else {
                // 重排之后还放不下，说明活着的条目本身就撑满了图集。
                // 丢掉这一个，下次用到时重新光栅化。
                continue;
            };
            for row in 0..h {
                let src = ((entry.rect[1] + row) * self.size + entry.rect[0]) as usize;
                let dst = ((rect[1] + row) * self.size + rect[0]) as usize;
                self.pixels[dst..dst + w as usize]
                    .copy_from_slice(&old_pixels[src..src + w as usize]);
            }
            entry.rect = rect;
            self.entries.insert(key, entry);
        }
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个 `w × h` 的纯色字形。
    fn glyph(w: u32, h: u32, value: u8) -> Vec<u8> {
        vec![value; (w * h) as usize]
    }

    fn image(w: u32, h: u32, value: u8, advance: f32) -> GlyphImage {
        GlyphImage {
            coverage: glyph(w, h, value),
            width: w,
            height: h,
            bearing_x: 0.0,
            bearing_y: 0.0,
            advance,
        }
    }

    fn insert(atlas: &mut GlyphAtlas, id: u16, w: u32, h: u32) -> Result<GlyphEntry, AtlasError> {
        atlas.insert(GlyphKey::new(0, id, 16.0), &image(w, h, 255, w as f32))
    }

    #[test]
    fn a_glyph_comes_back_out() {
        let mut atlas = GlyphAtlas::new(64);
        let key = GlyphKey::new(0, 42, 16.0);
        atlas
            .insert(
                key,
                &GlyphImage {
                    coverage: glyph(8, 12, 200),
                    width: 8,
                    height: 12,
                    bearing_x: 1.0,
                    bearing_y: 10.0,
                    advance: 9.0,
                },
            )
            .expect("应当装得下");

        let entry = atlas.get(&key).expect("刚插进去的应当查得到");
        assert_eq!(entry.rect[2..], [8, 12]);
        assert_eq!(entry.bearing_y, 10.0);
        assert_eq!(entry.advance, 9.0);
    }

    #[test]
    fn pixels_land_where_the_rect_says() {
        let mut atlas = GlyphAtlas::new(32);
        let key = GlyphKey::new(0, 1, 16.0);
        let entry = atlas
            .insert(key, &image(4, 4, 128, 4.0))
            .expect("应当装得下");

        let x = entry.rect[0];
        let y = entry.rect[1];
        for row in 0..4 {
            for col in 0..4 {
                let index = ((y + row) * atlas.size() + x + col) as usize;
                assert_eq!(atlas.pixels()[index], 128, "({row},{col}) 位置的像素不对");
            }
        }
    }

    #[test]
    fn glyphs_do_not_overlap() {
        let mut atlas = GlyphAtlas::new(64);
        let mut rects = Vec::new();
        for id in 0..20u16 {
            let entry = insert(&mut atlas, id, 6, 10).expect("应当装得下");
            rects.push(entry.rect);
        }

        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let disjoint = a[0] + a[2] <= b[0]
                    || b[0] + b[2] <= a[0]
                    || a[1] + a[3] <= b[1]
                    || b[1] + b[3] <= a[1];
                assert!(disjoint, "{a:?} 和 {b:?} 重叠了");
            }
        }
    }

    #[test]
    fn everything_stays_inside_the_atlas() {
        let mut atlas = GlyphAtlas::new(64);
        for id in 0..30u16 {
            let Ok(entry) = insert(&mut atlas, id, 5 + (id % 7) as u32, 9) else {
                continue;
            };
            assert!(entry.rect[0] + entry.rect[2] <= atlas.size());
            assert!(entry.rect[1] + entry.rect[3] <= atlas.size());
        }
    }

    #[test]
    fn size_is_part_of_the_key() {
        // 同一个字形在 12 px 和 48 px 下是两张位图。混为一谈的话，
        // 放大的那张会糊成一团。
        let mut atlas = GlyphAtlas::new(128);
        let small = GlyphKey::new(0, 7, 12.0);
        let large = GlyphKey::new(0, 7, 48.0);
        assert_ne!(small, large);

        atlas.insert(small, &image(6, 8, 255, 6.0)).unwrap();
        assert!(atlas.peek(&large).is_none());
    }

    #[test]
    fn near_identical_sizes_share_an_entry() {
        // 12.0 和 12.000001 不该是两个条目——浮点噪声会把图集撑爆。
        assert_eq!(GlyphKey::new(0, 1, 12.0), GlyphKey::new(0, 1, 12.000001));
    }

    #[test]
    fn a_blank_glyph_is_still_recorded() {
        // 空格没有位图。不记条目的话每帧都要重新光栅化一次。
        let mut atlas = GlyphAtlas::new(32);
        let key = GlyphKey::new(0, 3, 16.0);
        let entry = atlas
            .insert(
                key,
                &GlyphImage {
                    advance: 5.0,
                    ..Default::default()
                },
            )
            .expect("空白字形也该被接受");

        assert!(entry.is_blank());
        assert_eq!(entry.advance, 5.0);
        assert!(atlas.peek(&key).is_some());
    }

    #[test]
    fn an_oversized_glyph_is_rejected_without_evicting() {
        // 比图集还大的字形，驱逐再多次也放不下。反复驱逐只会把
        // 别人的字形全清光，还是失败。
        let mut atlas = GlyphAtlas::new(32);
        insert(&mut atlas, 1, 8, 8).unwrap();
        atlas.begin_frame();

        let err = insert(&mut atlas, 2, 64, 64).unwrap_err();
        assert_eq!(err, AtlasError::TooLarge);
        assert_eq!(atlas.evictions(), 0, "不该为放不下的字形做无用驱逐");
        assert_eq!(atlas.len(), 1, "原有的字形不该被清掉");
    }

    #[test]
    fn a_full_atlas_evicts_the_least_recently_used() {
        // 这是中文能不能用的关键：图集满了要淘汰旧字，而不是不画新字。
        let mut atlas = GlyphAtlas::new(32);

        let mut inserted = Vec::new();
        for id in 0..200u16 {
            atlas.begin_frame();
            if insert(&mut atlas, id, 6, 6).is_ok() {
                inserted.push(id);
            }
        }

        assert!(atlas.evictions() > 0, "满了之后应当发生驱逐");
        assert!(
            inserted.len() > 50,
            "只成功插入了 {} 个，驱逐没起作用",
            inserted.len()
        );
        // 最后插入的一定还在。
        let last = *inserted.last().unwrap();
        assert!(atlas.peek(&GlyphKey::new(0, last, 16.0)).is_some());
    }

    #[test]
    fn recently_used_glyphs_survive_eviction() {
        let mut atlas = GlyphAtlas::new(32);
        let hot = GlyphKey::new(0, 999, 16.0);
        atlas.insert(hot, &image(6, 6, 255, 6.0)).unwrap();

        for id in 0..40u16 {
            atlas.begin_frame();
            // 每帧都碰一下热字形，它就不该被淘汰。
            atlas.get(&hot);
            let _ = insert(&mut atlas, id, 6, 6);
        }

        assert!(atlas.peek(&hot).is_some(), "每帧都在用的字形被淘汰了");
    }

    #[test]
    fn glyphs_used_this_frame_are_never_evicted() {
        // 淘汰正在画的字形会导致同一帧内反复插入-淘汰，画面上是文字闪烁。
        let mut atlas = GlyphAtlas::new(24);
        atlas.begin_frame();

        let mut ok = 0;
        for id in 0..100u16 {
            // 注意：整个循环都在**同一帧**里。
            if insert(&mut atlas, id, 6, 6).is_ok() {
                ok += 1;
            }
        }
        assert!(ok > 0);
        assert_eq!(atlas.evictions(), 0, "同一帧内不该发生驱逐");
    }

    #[test]
    fn the_version_changes_only_when_pixels_change() {
        let mut atlas = GlyphAtlas::new(32);
        let before = atlas.version();

        // 纯查询不改像素。
        atlas.begin_frame();
        atlas.get(&GlyphKey::new(0, 1, 16.0));
        assert_eq!(atlas.version(), before, "查询不该让渲染器重传纹理");

        insert(&mut atlas, 1, 6, 6).unwrap();
        assert!(atlas.version() > before, "写入之后渲染器要重传");
    }

    #[test]
    fn uv_covers_the_rect() {
        let mut atlas = GlyphAtlas::new(64);
        let entry = insert(&mut atlas, 1, 16, 32).unwrap();
        let uv = entry.uv(atlas.size());

        assert!((uv[2] - uv[0] - 16.0 / 64.0).abs() < 1e-6, "U 跨度不对");
        assert!((uv[3] - uv[1] - 32.0 / 64.0).abs() < 1e-6, "V 跨度不对");
        assert!(uv.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn shelves_pick_the_closest_height() {
        // 把矮字塞进高货架会白白浪费一整条。开一张窄图集，
        // 先造一个高货架再插矮字，矮字应当另开货架而不是挤进去。
        let mut atlas = GlyphAtlas::new(64);
        let tall = insert(&mut atlas, 1, 8, 30).unwrap();
        let short = insert(&mut atlas, 2, 8, 6).unwrap();
        assert_ne!(tall.rect[1], short.rect[1], "矮字挤进了高货架");
    }

    #[test]
    fn the_white_texel_is_reserved_and_stays_white() {
        // 纯色矩形靠采样这一块出颜色。被字形覆盖或者被清空之后不补，
        // 界面上所有纯色块会一起变透明。
        let mut atlas = GlyphAtlas::new(64);
        let uv = atlas.white_uv();
        let x = (uv[0] * 64.0) as u32;
        let y = (uv[1] * 64.0) as u32;
        assert_eq!(atlas.pixels()[(y * 64 + x) as usize], 255);

        for id in 0..40u16 {
            atlas.begin_frame();
            let _ = insert(&mut atlas, id, 6, 6);
        }
        assert_eq!(
            atlas.pixels()[(y * 64 + x) as usize],
            255,
            "白块被字形盖掉了"
        );

        atlas.clear();
        assert_eq!(
            atlas.pixels()[(y * 64 + x) as usize],
            255,
            "清空后白块没补回来"
        );
    }

    #[test]
    fn clear_wipes_everything() {
        let mut atlas = GlyphAtlas::new(32);
        insert(&mut atlas, 1, 6, 6).unwrap();
        let version = atlas.version();

        atlas.clear();

        assert!(atlas.is_empty());
        // 白块是保留区，不算「内容」——除它之外应当全零。
        let white = GlyphAtlas::WHITE_TEXEL;
        for y in 0..atlas.size() {
            for x in 0..atlas.size() {
                if x < white && y < white {
                    continue;
                }
                assert_eq!(atlas.pixels()[(y * atlas.size() + x) as usize], 0);
            }
        }
        assert!(atlas.version() > version);
    }
}
