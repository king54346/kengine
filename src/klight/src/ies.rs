//! IES 配光曲线（IESNA LM-63）的解析与烘焙。
//!
//! # 这是什么
//!
//! 灯具厂商给的一张**角度 → 光强**表。现实里没有一盏灯是完美的圆锥：
//! 手电筒中心有暗斑，射灯边缘有环，筒灯的光往下收、往侧面几乎不出。
//! 这些形状测量出来存成 `.ies` 文件，是照明设计里的通用交换格式。
//!
//! 引擎这边不需要新的光照路径——[`IesProfile::bake_cookie`] 把这张表
//! 烘成一张 cookie 贴图，接到已有的聚光灯 cookie 上就行。
//!
//! # 格式为什么啰嗦
//!
//! LM-63 有 1986 / 1991 / 1995 / 2002 四个版本，差别在头部：有没有版本行、
//! 关键字用不用方括号。**数值部分四版一样**，但有一条要命的性质：
//!
//! > 数值之间只按**空白**分隔，和换行完全无关。
//!
//! 同一份数据，有的文件一行一个角度，有的把三千个坎德拉值挤成十几行。
//! 所以不能按行解析，只能先切成词流再按位置读。这一条是这个格式最容易
//! 踩的坑：按行写的解析器在半数文件上能跑，剩下半数读出一堆垃圾——
//! 而垃圾光强不会报错，只是灯的形状不对。
//!
//! # 没做的部分
//!
//! - **TILT 的倾斜修正**：`TILT=INCLUDE` 的数据块会被正确**跳过**，
//!   但不参与计算。它修正的是「灯具装歪了之后光输出的衰减」，
//!   是安装参数不是光形，实时渲染里几乎没人用。
//! - **B 型和 A 型光度学**（`photometric_type` 2 和 3）：解析出来了，
//!   但采样按 C 型处理。这两种用在汽车前照灯和体育场投光灯上，
//!   它们的角度定义是另一套坐标系。用错了形状会歪，所以
//!   [`IesProfile::photometric_type`] 暴露出来让调用方自己判断。

use std::collections::VecDeque;

/// 解析失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IesError(pub String);

impl std::fmt::Display for IesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IES 解析失败：{}", self.0)
    }
}

impl std::error::Error for IesError {}

/// 一条解析好的配光曲线。
///
/// 角度全部是**度**，光强的单位是坎德拉（cd），已经乘过文件里的
/// 各种倍率因子。
#[derive(Debug, Clone, PartialEq)]
pub struct IesProfile {
    /// 垂直角，递增。C 型光度学里 0° 是正下方（灯轴方向），
    /// 90° 是水平，180° 是正上方。
    vertical: Vec<f32>,
    /// 水平角（方位角），递增。只有一个值时是旋转对称的。
    horizontal: Vec<f32>,
    /// 光强表，`horizontal.len() * vertical.len()` 个，**水平角在外层**。
    candela: Vec<f32>,
    /// 光度学类型：1 = C，2 = B，3 = A。
    photometric_type: i32,
    /// 标称光通量（流明），文件里的 `lumens_per_lamp * number_of_lamps`。
    /// 负值在文件里表示「绝对光度学」，此时这个数没有意义。
    lumens: f32,
    /// 表里的最大光强。归一化要用它，缓存下来省得每次扫一遍。
    peak: f32,
}

impl IesProfile {
    /// 解析一份 `.ies` 的字节流。
    ///
    /// 按 latin-1 当文本读：LM-63 规定 ASCII，但实际文件的关键字里
    /// 常有厂商名字带的重音字符，按 UTF-8 读会直接失败。而那些字符
    /// 全在**头部关键字**里，和数值无关——所以宽容地读进来、
    /// 让它们随头部一起被丢掉，比因为一个 `é` 拒绝整份文件要好。
    pub fn parse(bytes: &[u8]) -> Result<Self, IesError> {
        let text: String = bytes.iter().map(|&b| b as char).collect();
        Self::parse_str(&text)
    }

    /// 解析一份 `.ies` 的文本。
    pub fn parse_str(text: &str) -> Result<Self, IesError> {
        // ── 1. 找 TILT 行 ──
        //
        // 它是头部和数值区的分界，也是唯一一个位置固定的标记。
        // 头部的关键字数量、有没有版本行，各版本都不一样，
        // 找 TILT 比逐版本猜要稳。
        let mut lines = text.lines();
        let mut tilt = None;
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed.len() >= 5 && trimmed[..5].eq_ignore_ascii_case("TILT=") {
                tilt = Some(trimmed[5..].trim().to_string());
                break;
            }
        }
        let Some(tilt) = tilt else {
            return Err(IesError("找不到 TILT= 行，这不像是一份 IES 文件".into()));
        };

        // ── 2. 剩下的全切成词 ──
        //
        // 逗号也当分隔符：LM-63 没这么规定，但用逗号分隔坎德拉值的
        // 文件确实存在。多认一种分隔符不会让合规的文件出错，
        // 而不认的话那些文件会把 `"1.0,2.0"` 读成一个数然后失败。
        let rest: String = lines.collect::<Vec<_>>().join("\n");
        let mut tokens: VecDeque<&str> = rest
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|token| !token.is_empty())
            .collect();

        let next_f32 = |tokens: &mut VecDeque<&str>, what: &str| -> Result<f32, IesError> {
            let token = tokens
                .pop_front()
                .ok_or_else(|| IesError(format!("读 {what} 时文件就结束了")))?;
            token
                .parse::<f32>()
                .map_err(|_| IesError(format!("{what} 不是个数：{token:?}")))
        };

        // ── 3. TILT=INCLUDE 的数据块 ──
        //
        // 不参与计算，但**必须消费掉**——留在词流里的话后面所有
        // 按位置读的字段会整体错位，而错位的光强表不会报错。
        if tilt.eq_ignore_ascii_case("INCLUDE") {
            let _geometry = next_f32(&mut tokens, "TILT 的灯具几何类型")?;
            let pairs = next_f32(&mut tokens, "TILT 的数据对数")? as usize;
            for _ in 0..pairs * 2 {
                next_f32(&mut tokens, "TILT 的角度/倍率")?;
            }
        }

        // ── 4. 两行固定字段 ──
        let lamps = next_f32(&mut tokens, "灯泡数量")?;
        let lumens_per_lamp = next_f32(&mut tokens, "每只灯泡的流明")?;
        let candela_multiplier = next_f32(&mut tokens, "坎德拉倍率")?;
        let vertical_count = next_f32(&mut tokens, "垂直角个数")? as usize;
        let horizontal_count = next_f32(&mut tokens, "水平角个数")? as usize;
        let photometric_type = next_f32(&mut tokens, "光度学类型")? as i32;
        let _units = next_f32(&mut tokens, "单位类型")?;
        let _width = next_f32(&mut tokens, "灯具宽")?;
        let _length = next_f32(&mut tokens, "灯具长")?;
        let _height = next_f32(&mut tokens, "灯具高")?;

        let ballast = next_f32(&mut tokens, "镇流器因子")?;
        // 1995 版之前这里是「镇流器-灯泡光度因子」，之后改成保留字段
        // 但仍然要占一个位置。两种情况都乘进去：保留字段按规定填 1。
        let ballast_photometric = next_f32(&mut tokens, "镇流器-灯泡光度因子")?;
        let _watts = next_f32(&mut tokens, "输入功率")?;

        if vertical_count == 0 || horizontal_count == 0 {
            return Err(IesError(format!(
                "角度个数不能为 0（垂直 {vertical_count}，水平 {horizontal_count}）"
            )));
        }

        // ── 5. 角度与光强 ──
        let mut vertical = Vec::with_capacity(vertical_count);
        for index in 0..vertical_count {
            vertical.push(next_f32(&mut tokens, &format!("第 {index} 个垂直角"))?);
        }
        let mut horizontal = Vec::with_capacity(horizontal_count);
        for index in 0..horizontal_count {
            horizontal.push(next_f32(&mut tokens, &format!("第 {index} 个水平角"))?);
        }

        // 所有倍率一次性乘进表里，采样时就不必再乘。
        // 倍率为 0 或负的文件是存在的（写错了），那种情况下整张表会变成
        // 0 或负数——夹到 0 而不是让负光强流进渲染器。
        let scale = candela_multiplier * ballast * ballast_photometric;
        let total = vertical_count * horizontal_count;
        let mut candela = Vec::with_capacity(total);
        for index in 0..total {
            let value = next_f32(&mut tokens, &format!("第 {index} 个光强"))?;
            candela.push((value * scale).max(0.0));
        }

        let peak = candela.iter().copied().fold(0.0f32, f32::max);

        Ok(Self {
            vertical,
            horizontal,
            candela,
            photometric_type,
            lumens: lamps * lumens_per_lamp,
            peak,
        })
    }

    /// 光度学类型：1 = C（最常见），2 = B，3 = A。
    ///
    /// 采样按 C 型处理。拿到 2 或 3 时形状会歪，调用方可以据此警告。
    pub fn photometric_type(&self) -> i32 {
        self.photometric_type
    }

    /// 标称光通量（流明）。负值表示文件用的是绝对光度学，这个数没意义。
    pub fn lumens(&self) -> f32 {
        self.lumens
    }

    /// 表里的最大光强（坎德拉）。
    ///
    /// [`bake_cookie`](Self::bake_cookie) 按它归一化，所以想让画面里的
    /// 亮度和真实灯具对得上时，灯的强度要按这个数去设。
    pub fn peak_candela(&self) -> f32 {
        self.peak
    }

    /// 垂直角的覆盖范围（度）。
    pub fn vertical_range(&self) -> (f32, f32) {
        (
            self.vertical.first().copied().unwrap_or(0.0),
            self.vertical.last().copied().unwrap_or(0.0),
        )
    }

    /// 某个方向上的光强（坎德拉）。
    ///
    /// `vertical` 是离灯轴的夹角，`horizontal` 是绕灯轴的方位角，都是度。
    /// 超出表格范围的垂直角**夹取**到边界而不是返回 0：很多文件只测到
    /// 90°，而 90° 之外确实没有光，夹取和补零的结果一样；但对只测了
    /// 0–60° 的窄光束灯具，补零会在 60° 处切出一道硬边。
    pub fn sample(&self, vertical: f32, horizontal: f32) -> f32 {
        let (v_index, v_fraction) = interpolate(&self.vertical, vertical);
        let (h_index, h_fraction) = interpolate(&self.horizontal, self.fold_horizontal(horizontal));

        let at = |h: usize, v: usize| -> f32 {
            let h = h.min(self.horizontal.len() - 1);
            let v = v.min(self.vertical.len() - 1);
            self.candela[h * self.vertical.len() + v]
        };

        let low = lerp(at(h_index, v_index), at(h_index, v_index + 1), v_fraction);
        let high = lerp(
            at(h_index + 1, v_index),
            at(h_index + 1, v_index + 1),
            v_fraction,
        );
        lerp(low, high, h_fraction)
    }

    /// 按文件声明的水平角范围做对称展开，把任意方位角折进表格里。
    ///
    /// LM-63 用**水平角的跨度**隐式声明对称性，没有单独的字段：
    ///
    /// | 跨度 | 含义 | 折叠 |
    /// |---|---|---|
    /// | 只有一个角 | 旋转对称（筒灯、球泡） | 全部落在那一个角 |
    /// | 0–90 | 两个轴都对称（方形灯具） | 折进第一象限 |
    /// | 0–180 | 左右对称（大部分线性灯具） | 折进上半 |
    /// | 0–360 | 完全不对称 | 只取模 |
    ///
    /// 折错了灯的形状会左右颠倒或转 90°，而这在动态场景里看不出来。
    fn fold_horizontal(&self, angle: f32) -> f32 {
        if self.horizontal.len() <= 1 {
            return self.horizontal.first().copied().unwrap_or(0.0);
        }
        let span = self.horizontal[self.horizontal.len() - 1] - self.horizontal[0];
        let mut folded = angle.rem_euclid(360.0);

        if span <= 180.5 {
            // 左右对称：180..360 映回 180..0。
            if folded > 180.0 {
                folded = 360.0 - folded;
            }
        }
        if span <= 90.5 {
            // 四象限对称：再把 90..180 映回 90..0。
            if folded > 90.0 {
                folded = 180.0 - folded;
            }
        }
        folded
    }

    /// 光强衰减到峰值的 `threshold` 之前，最远的那个垂直角（度）。
    ///
    /// 用来定聚光灯的外锥角：烘 cookie 时锥角给大了，图案中间一小块
    /// 亮、四周全黑，等于白白浪费分辨率；给小了则把有光的部分切掉。
    ///
    /// `threshold` 一般给 0.01。返回值夹在 `[1, 89.9]`——
    /// [`crate::Light::spot`] 的锥角上限就是 89.9。
    pub fn cone_angle(&self, threshold: f32) -> f32 {
        if self.peak <= 0.0 {
            return 45.0;
        }
        let cutoff = self.peak * threshold.clamp(0.0, 1.0);
        // 从最外圈往里找第一个还够亮的角。从里往外找会被中间的暗斑
        // 骗到——手电筒中心暗斑那类光形在轴上就低于阈值。
        for (index, &angle) in self.vertical.iter().enumerate().rev() {
            let brightest = (0..self.horizontal.len())
                .map(|h| self.candela[h * self.vertical.len() + index])
                .fold(0.0f32, f32::max);
            if brightest >= cutoff {
                return angle.clamp(1.0, 89.9);
            }
        }
        45.0
    }

    /// 把配光曲线烘成一张方形的 cookie 贴图，RGBA8，长度 `size * size * 4`。
    ///
    /// `cone_angle` 必须和这张 cookie 要挂的那盏聚光灯的**外锥半角**
    /// 一致（度）。不一致的话图案会被拉伸或裁掉，而画面上只是
    /// 「光斑大小不太对」，看不出是哪里错了。
    ///
    /// # 半径和角度的关系
    ///
    /// 着色器把着色点投到光的成像平面上、再除以外锥在那个距离上的半径
    /// 得到 UV（见 `light.wgsl` 的 `light_cookie_uv`）。所以贴图上
    /// 半径 `r` 处对应的离轴角是：
    ///
    /// ```text
    /// θ = atan(r * tan(cone_angle))
    /// ```
    ///
    /// **不是** `θ = r * cone_angle`。写成线性的话小角度下几乎看不出差别，
    /// 大锥角下光形会被明显压扁——而这种错没有任何东西会报出来。
    pub fn bake_cookie(&self, size: u32, cone_angle: f32) -> Vec<u8> {
        let size = size.max(1);
        let tan_cone = cone_angle.clamp(0.1, 89.9).to_radians().tan();
        // 峰值为 0 的表（全黑的文件）会让归一化除零，退化成全黑。
        let inverse_peak = if self.peak > 0.0 { 1.0 / self.peak } else { 0.0 };

        let mut data = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                // 贴图坐标 → [-1, 1]。v 取反：贴图的 v 向下，而
                // `light_cookie_uv` 里的上轴向上，两边必须一致。
                let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                let v = 1.0 - (y as f32 + 0.5) / size as f32 * 2.0;
                let radius = (u * u + v * v).sqrt();

                // 锥外一律 0。方形贴图的四角会被投到锥外，不抹掉的话
                // 墙上会出现一圈方形的亮边。
                let value = if radius > 1.0 {
                    0.0
                } else {
                    let vertical = (radius * tan_cone).atan().to_degrees();
                    let horizontal = v.atan2(u).to_degrees();
                    (self.sample(vertical, horizontal) * inverse_peak).clamp(0.0, 1.0)
                };

                // 灰度：IES 只测光强不测颜色，颜色由灯自己的 `color` 给。
                let byte = (value * 255.0 + 0.5) as u8;
                data.extend_from_slice(&[byte, byte, byte, 255]);
            }
        }
        data
    }
}

/// 在一张递增的角度表里定位：返回下标和到下一个下标的插值系数。
///
/// 超出范围时夹到边界（系数 0），而不是外推——外推出来的光强可以是负的。
fn interpolate(table: &[f32], value: f32) -> (usize, f32) {
    if table.len() <= 1 {
        return (0, 0.0);
    }
    if value <= table[0] {
        return (0, 0.0);
    }
    if value >= table[table.len() - 1] {
        return (table.len() - 1, 0.0);
    }
    // 表通常只有几十项，线性扫描比二分快也更好读。
    for index in 0..table.len() - 1 {
        if value < table[index + 1] {
            let span = table[index + 1] - table[index];
            let fraction = if span > 0.0 {
                (value - table[index]) / span
            } else {
                0.0
            };
            return (index, fraction);
        }
    }
    (table.len() - 1, 0.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// [`IesProfile`] 的资源类型 id。
pub const IES_TYPE_UUID: kcore::uuid::Uuid =
    kcore::uuid::uuid!("2f8c1d3a-6b47-4e59-9c2a-7d1e5f4b8a06");

impl kasset::ResourceData for IesProfile {
    fn type_uuid(&self) -> kcore::uuid::Uuid {
        IES_TYPE_UUID
    }
}

/// 加载 `.ies` 配光曲线。
///
/// 加载出来的是**曲线本身**而不是烘好的贴图：烘的时候要知道挂它的那盏
/// 聚光灯用多大的外锥角，而那是场景说了算的，加载器不知道。
pub struct IesLoader;

impl kasset::ResourceLoader for IesLoader {
    fn extensions(&self) -> &[&str] {
        &["ies"]
    }

    fn data_type_uuid(&self) -> kcore::uuid::Uuid {
        IES_TYPE_UUID
    }

    fn load(
        &self,
        path: std::path::PathBuf,
        io: std::sync::Arc<dyn kasset::ResourceIo>,
    ) -> kasset::BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let profile = IesProfile::parse(&bytes).map_err(kasset::LoadError::custom)?;

            klog::debug!(
                "IES 已解析：{}（垂直角 {:.0}..{:.0}°，峰值 {:.0} cd）",
                path.display(),
                profile.vertical_range().0,
                profile.vertical_range().1,
                profile.peak_candela(),
            );

            Ok(Box::new(profile) as Box<dyn kasset::ResourceData>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份最小但合规的 IES：旋转对称，5 个垂直角，光强从 1000 线性降到 0。
    fn simple() -> String {
        [
            "IESNA:LM-63-2002",
            "[TEST] 合成的",
            "[MANUFAC] kengine",
            "TILT=NONE",
            "1 1000 1 5 1 1 1 0.3 0.3 0.0",
            "1.0 1.0 40",
            "0 22.5 45 67.5 90",
            "0",
            "1000 750 500 250 0",
        ]
        .join("\n")
    }

    #[test]
    fn a_minimal_file_parses() {
        let profile = IesProfile::parse_str(&simple()).expect("该解析得出来");
        assert_eq!(profile.vertical_range(), (0.0, 90.0));
        assert_eq!(profile.peak_candela(), 1000.0);
        assert_eq!(profile.photometric_type(), 1);
        assert_eq!(profile.lumens(), 1000.0);
    }

    #[test]
    fn values_may_wrap_across_lines_however_they_like() {
        // 这是这个格式最容易踩的坑：数值只按空白分隔，和换行无关。
        // 按行解析的话，同一份数据换个排版就读出一堆垃圾——
        // 而垃圾光强不报错，只是灯的形状不对。
        let squashed = [
            "TILT=NONE",
            "1 1000 1 5 1",
            "1 1 0.3",
            "0.3 0.0 1.0 1.0",
            "40 0 22.5",
            "45",
            "67.5 90 0 1000 750",
            "500 250 0",
        ]
        .join("\n");
        let a = IesProfile::parse_str(&simple()).unwrap();
        let b = IesProfile::parse_str(&squashed).unwrap();
        assert_eq!(a, b, "同样的数据换个排版就读出了不一样的结果");
    }

    #[test]
    fn commas_count_as_separators() {
        // 不合规但确实存在的写法。多认一种分隔符不会让合规文件出错。
        let text = simple().replace("1000 750 500 250 0", "1000,750,500,250,0");
        let profile = IesProfile::parse_str(&text).expect("逗号分隔的也该读得进来");
        assert_eq!(profile.sample(45.0, 0.0), 500.0);
    }

    #[test]
    fn the_tilt_block_is_skipped_not_misread() {
        // TILT=INCLUDE 的数据块不参与计算，但**必须消费掉**。
        // 留在词流里的话后面所有字段整体错位，而错位不报错。
        let text = [
            "TILT=INCLUDE",
            "1",
            "3",
            "0 45 90",
            "1.0 0.9 0.5",
            "1 1000 1 5 1 1 1 0.3 0.3 0.0",
            "1.0 1.0 40",
            "0 22.5 45 67.5 90",
            "0",
            "1000 750 500 250 0",
        ]
        .join("\n");
        let with_tilt = IesProfile::parse_str(&text).expect("带 TILT 块的也该读得进来");
        let without = IesProfile::parse_str(&simple()).unwrap();
        assert_eq!(
            with_tilt.candela, without.candela,
            "TILT 块没被跳干净，后面的字段整体错位了"
        );
    }

    #[test]
    fn all_the_multipliers_are_folded_into_the_table() {
        // 坎德拉倍率、镇流器因子、镇流器-灯泡光度因子三个都要乘。
        // 漏乘一个，灯的绝对亮度就差一个常数倍——而画面上只是「有点暗」。
        let text = [
            "TILT=NONE",
            // 坎德拉倍率 2
            "1 1000 2 2 1 1 1 0 0 0",
            // 镇流器 0.5，镇流器-灯泡 3
            "0.5 3 40",
            "0 90",
            "0",
            "100 0",
        ]
        .join("\n");
        let profile = IesProfile::parse_str(&text).unwrap();
        assert_eq!(profile.peak_candela(), 100.0 * 2.0 * 0.5 * 3.0);
    }

    #[test]
    fn sampling_interpolates_between_table_entries() {
        let profile = IesProfile::parse_str(&simple()).unwrap();
        assert_eq!(profile.sample(0.0, 0.0), 1000.0);
        assert_eq!(profile.sample(45.0, 0.0), 500.0);
        // 表格点之间线性插值。
        assert!((profile.sample(33.75, 0.0) - 625.0).abs() < 1e-3);
    }

    #[test]
    fn out_of_range_angles_clamp_instead_of_dropping_to_zero() {
        // 补零的话，只测到 60° 的窄光束灯具会在 60° 处切出一道硬边。
        let profile = IesProfile::parse_str(&simple()).unwrap();
        assert_eq!(profile.sample(-10.0, 0.0), 1000.0);
        assert_eq!(profile.sample(200.0, 0.0), 0.0);
    }

    #[test]
    fn a_single_horizontal_angle_means_rotationally_symmetric() {
        let profile = IesProfile::parse_str(&simple()).unwrap();
        // 方位角怎么转都该是同一个值。
        for azimuth in [0.0, 37.0, 90.0, 180.0, 271.0, 359.0] {
            assert_eq!(profile.sample(45.0, azimuth), 500.0, "方位角 {azimuth} 处变了");
        }
    }

    #[test]
    fn the_horizontal_span_declares_the_symmetry() {
        // LM-63 没有对称性字段，是靠水平角的**跨度**隐式声明的。
        // 折错了灯的形状会左右颠倒或转 90°，而这在动态场景里看不出来。
        //
        // 0–90 的跨度 = 四象限对称。
        let quadrant = [
            "TILT=NONE",
            "1 1000 1 2 3 1 1 0 0 0",
            "1 1 40",
            "0 90",
            "0 45 90",
            // h=0 那一列全 100，h=45 全 200，h=90 全 300
            "100 100 200 200 300 300",
        ]
        .join("\n");
        let profile = IesProfile::parse_str(&quadrant).unwrap();

        assert_eq!(profile.sample(0.0, 0.0), 100.0);
        assert_eq!(profile.sample(0.0, 90.0), 300.0);
        // 135° 折进第一象限 → 45°
        assert_eq!(profile.sample(0.0, 135.0), 200.0);
        // 315° 先折成 45°
        assert_eq!(profile.sample(0.0, 315.0), 200.0);
        // 180° 折成 0°
        assert_eq!(profile.sample(0.0, 180.0), 100.0);
    }

    #[test]
    fn a_bilateral_profile_folds_only_the_upper_half() {
        let bilateral = [
            "TILT=NONE",
            "1 1000 1 1 3 1 1 0 0 0",
            "1 1 40",
            "0",
            "0 90 180",
            "100 200 300",
        ]
        .join("\n");
        let profile = IesProfile::parse_str(&bilateral).unwrap();
        assert_eq!(profile.sample(0.0, 90.0), 200.0);
        // 270° 映回 90°，而**不该**再折成 0。
        assert_eq!(profile.sample(0.0, 270.0), 200.0);
        assert_eq!(profile.sample(0.0, 180.0), 300.0);
    }

    #[test]
    fn a_file_without_a_tilt_line_is_rejected() {
        // 与其读出一堆垃圾光强，不如直接说这不是 IES。
        assert!(IesProfile::parse_str("1 2 3 4 5").is_err());
    }

    #[test]
    fn a_truncated_file_is_rejected() {
        // 截断的文件如果只是「少读几个光强」，剩下的会被填成 0——
        // 灯的一半会莫名其妙地黑掉，而且不报错。
        let text = ["TILT=NONE", "1 1000 1 5 1 1 1 0 0 0", "1 1 40", "0 22.5"].join("\n");
        assert!(IesProfile::parse_str(&text).is_err());
    }

    #[test]
    fn the_cone_angle_covers_the_lit_part() {
        let profile = IesProfile::parse_str(&simple()).unwrap();
        // 光强在 90° 处才降到 0，67.5° 处还有峰值的 25%。
        assert_eq!(profile.cone_angle(0.01), 67.5);
        // 阈值提到 60% 之后只剩 22.5° 那一圈够亮。
        assert_eq!(profile.cone_angle(0.6), 22.5);
    }

    #[test]
    fn the_cone_angle_is_found_from_the_outside_in() {
        // 手电筒那类「中心有暗斑」的光形，从里往外找会在第一个
        // 低于阈值的角上停住，把外面还亮着的一圈整个切掉。
        let donut = [
            "TILT=NONE",
            "1 1000 1 4 1 1 1 0 0 0",
            "1 1 40",
            "0 20 40 60",
            "0",
            // 轴上是暗斑，20..40 才是亮的
            "10 1000 800 5",
        ]
        .join("\n");
        let profile = IesProfile::parse_str(&donut).unwrap();
        assert_eq!(profile.cone_angle(0.1), 40.0, "从里往外找会停在 0° 那个暗斑上");
    }

    #[test]
    fn the_baked_radius_maps_through_a_tangent_not_a_straight_line() {
        // 半径和角度的关系是 θ = atan(r · tan(cone))，不是 θ = r · cone。
        // 写成线性的话小角度下几乎看不出差别，大锥角下光形被压扁——
        // 而这种错没有任何东西会报出来，所以这里把它钉死。
        let profile = IesProfile::parse_str(&simple()).unwrap();
        let size = 64u32;
        let cone = 60.0f32;
        let data = profile.bake_cookie(size, cone);
        assert_eq!(data.len() as u32, size * size * 4);

        // 取正中央右边、半径正好 0.5 的那个像素。
        let center = size / 2;
        let target_radius = 0.5f32;
        // 像素中心的 u = (x + 0.5) / size * 2 - 1，反解 x。
        let x = ((target_radius + 1.0) * size as f32 / 2.0 - 0.5).round() as u32;
        let y = center;
        let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
        let v = 1.0 - (y as f32 + 0.5) / size as f32 * 2.0;
        let radius = (u * u + v * v).sqrt();

        let expected_angle = (radius * cone.to_radians().tan()).atan().to_degrees();
        let expected = profile.sample(expected_angle, 0.0) / profile.peak_candela();
        let actual = data[((y * size + x) * 4) as usize] as f32 / 255.0;
        assert!(
            (actual - expected).abs() < 0.01,
            "半径 {radius:.3} 处该是 {expected:.3}（离轴 {expected_angle:.1}°），实际 {actual:.3}"
        );

        // 线性映射会给出明显不同的值——确认这条测试真的分得开两者。
        let linear_angle = radius * cone;
        let linear = profile.sample(linear_angle, 0.0) / profile.peak_candela();
        assert!(
            (linear - expected).abs() > 0.03,
            "线性和正切在这个配置下差别太小，这条测试分不开两者"
        );
    }

    #[test]
    fn the_baked_cookie_is_black_outside_the_cone() {
        // 方形贴图的四角会被投到锥外。不抹掉的话墙上会出现方形的亮边。
        let profile = IesProfile::parse_str(&simple()).unwrap();
        let size = 32u32;
        let data = profile.bake_cookie(size, 30.0);
        // 左上角。
        assert_eq!(data[0], 0);
        // 右下角。
        let last = ((size * size - 1) * 4) as usize;
        assert_eq!(data[last], 0);
        // 正中央该是峰值。
        let center = ((size / 2 * size + size / 2) * 4) as usize;
        assert!(data[center] > 200, "中央该接近峰值，实际 {}", data[center]);
    }

    /// 仓库里那几份**真的**厂商 IES 文件。
    ///
    /// 合成的样例只能验「我以为的格式」，验不了「厂商实际写出来的格式」。
    /// 这几份来自 three.js 的例子素材（BEGA 等厂商的实测数据），
    /// 排版、角度数量、光度学类型各不相同。
    /// 用 `include_bytes!` 而不是 `include_str!`：这些文件不保证是 UTF-8
    /// （厂商名字里的重音字符按 latin-1 存），而 `include_str!` 会直接
    /// 编译失败。走 `parse` 那条路也正好和运行时的加载路径一致。
    const REAL_FILES: [&[u8]; 4] = [
        include_bytes!("../../../examples/threejs/ies/007cfb11e343e2f42e3b476be4ab684e.ies"),
        include_bytes!("../../../examples/threejs/ies/02a7562c650498ebb301153dbbf59207.ies"),
        include_bytes!("../../../examples/threejs/ies/06b4cfdc8805709e767b5e2e904be8ad.ies"),
        include_bytes!("../../../examples/threejs/ies/1a936937a49c63374e6d4fbed9252b29.ies"),
    ];

    #[test]
    fn the_real_manufacturer_files_all_parse() {
        // 合成的样例只验得了「我以为的格式」。厂商的文件才有那些真实的
        // 花样：73 个垂直角挤成 6 行、流明写成 -1（绝对光度学）、
        // 头部关键字里带空格。
        for (index, bytes) in REAL_FILES.iter().enumerate() {
            let profile = IesProfile::parse(bytes)
                .unwrap_or_else(|error| panic!("第 {index} 份厂商文件读不进来：{error}"));

            assert!(
                profile.peak_candela() > 0.0,
                "第 {index} 份的峰值光强是 0 —— 表多半读错位了"
            );
            let (low, high) = profile.vertical_range();
            assert!(
                (0.0..=180.0).contains(&low) && (0.0..=180.0).contains(&high) && high > low,
                "第 {index} 份的垂直角范围离谱：{low}..{high}"
            );
            // 错位最典型的症状就是把角度读成了光强。角度不会超过 360，
            // 而这几份的峰值都在几百到几千坎德拉。
            assert!(
                profile.cone_angle(0.01) >= 1.0,
                "第 {index} 份算不出锥角"
            );
        }
    }

    #[test]
    fn a_real_file_bakes_without_holes() {
        // 烘出来整张全黑，说明角度或归一化错了——而画面上只是
        // 「这盏灯不亮」，看起来像忘了打开。
        for (index, bytes) in REAL_FILES.iter().enumerate() {
            let profile = IesProfile::parse(bytes).unwrap();
            let cone = profile.cone_angle(0.01);
            let data = profile.bake_cookie(64, cone);
            let brightest = data.iter().step_by(4).copied().max().unwrap_or(0);
            assert!(
                brightest > 200,
                "第 {index} 份烘出来最亮才 {brightest}，锥角 {cone}° —— 整张图几乎全黑"
            );
        }
    }

    #[test]
    fn a_rotationally_symmetric_profile_bakes_to_a_symmetric_image() {
        // 烘出来的图不对称，说明方位角的取法和贴图坐标对不上。
        let profile = IesProfile::parse_str(&simple()).unwrap();
        let size = 33u32; // 奇数，正中间有一个像素
        let data = profile.bake_cookie(size, 45.0);
        let at = |x: u32, y: u32| data[((y * size + x) * 4) as usize];

        for offset in 1..=(size / 2) {
            let mid = size / 2;
            let left = at(mid - offset, mid);
            let right = at(mid + offset, mid);
            let up = at(mid, mid - offset);
            let down = at(mid, mid + offset);
            assert_eq!(left, right, "偏移 {offset} 处左右不对称");
            assert_eq!(left, up, "偏移 {offset} 处水平和竖直不对称");
            assert_eq!(up, down, "偏移 {offset} 处上下不对称");
        }
    }
}
