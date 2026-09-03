//! 环境捕获：把场景从一个点往六个方向各渲一遍，拼成一张等距柱状 HDR。
//!
//! # 这是干什么用的
//!
//! 反射探针和光照探针都要一张「站在这里往四周看是什么样」的环境图。
//! 在这之前，那张图只能是现成的 `.hdr` 文件或者程序生成的——
//! 也就是说探针**照不出场景里真实的东西**：红房间的墙是红的，
//! 但只有当你手工造一张红色的环境图时它才反射出红色。
//!
//! 捕获让探针照出场景本身。走一遍完整的渲染管线，所以直射光、阴影、
//! 天空、自发光都在里面。
//!
//! # 一次弹射
//!
//! 捕获时用的是**当前**的环境（全局环境或已有的探针）。所以捕出来的
//! 图里，镜面物体反射的是旧环境而不是新环境。这是所有实时探针烘焙的
//! 常规做法，也是 three.js 的 `CubeCamera` 的做法：想要多次弹射就多捕
//! 几遍，每遍都比上一遍准一点。
//!
//! # 不走后处理
//!
//! 色调映射和 bloom 是给屏幕看的。过一遍再当环境用，亮部会被压掉，
//! 反射里的高光全没了。所以捕获在主 pass 画完就把 HDR 目标拷走，
//! 拿到的是线性辐射亮度。
//!
//! # 接缝
//!
//! 双线性插值要取四个相邻纹素，而落在面边缘上的那些**有一半在隔壁面里**。
//! 夹到边上（第一版的做法）会让两个面各自把自己最外圈的值往外抹一格，
//! 交界处就留下一道一格宽的脊。
//!
//! 现在越界的那个纹素会**到相邻面里去取**：把它的中心方向算出来，
//! 交给包含这个方向的那一面。不需要邻接表——方向本身就说明了它属于谁。

use kmath::{Mat4, Vec3};

/// 立方体的六个面：朝向，以及决定「上」在哪的参考向量。
///
/// 上方向的选法只要**自洽**就行——渲染和采样用的是同一份表，
/// 所以面内怎么转都不影响最终结果。但正上和正下两个面不能再用 +Y
/// 当参考：那时朝向和参考共线，叉乘出来是零向量，整个面会变成 NaN。
pub(crate) const FACES: [(Vec3, Vec3); 6] = [
    (Vec3::X, Vec3::Y),
    (Vec3::NEG_X, Vec3::Y),
    (Vec3::Y, Vec3::Z),
    (Vec3::NEG_Y, Vec3::NEG_Z),
    (Vec3::Z, Vec3::Y),
    (Vec3::NEG_Z, Vec3::Y),
];

/// 一个面的正交基：朝向、右、上。
///
/// 和 `Mat4::look_to_rh` 建视图矩阵时用的是同一套算法（右 = 前 × 上参考，
/// 上 = 右 × 前）。两边必须一致：渲染按一套基、采样按另一套的话，
/// 拼出来的全景图会一格一格地错位，而**不会报任何错**。
pub(crate) fn face_basis(index: usize) -> (Vec3, Vec3, Vec3) {
    let (forward, up_hint) = FACES[index];
    let right = forward.cross(up_hint).normalize();
    let up = right.cross(forward);
    (forward, right, up)
}

/// 某个面的「相机到世界」矩阵。
pub(crate) fn face_camera_to_world(position: Vec3, index: usize) -> Mat4 {
    let (forward, _) = FACES[index];
    Mat4::look_to_rh(position, forward, FACES[index].1).inverse()
}

/// 一个面上的一块 RGB 数据，行主序。
pub(crate) struct Face {
    pub size: usize,
    /// `size * size * 3` 个浮点。
    pub pixels: Vec<f32>,
}

impl Face {
    /// 取一个纹素。下标必须在范围内。
    fn texel(&self, x: usize, y: usize) -> Vec3 {
        let index = (y * self.size + x) * 3;
        Vec3::new(
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
        )
    }
}

/// 一个面上某个纹素中心的方向。
///
/// `x`、`y` **允许越界**——越界的那个「纹素」在几何上就落在相邻面里，
/// 这正是跨面取样要用的。
fn texel_direction(index: usize, x: f32, y: f32, size: usize) -> Vec3 {
    let (forward, right, up) = face_basis(index);
    let size = size as f32;
    let ndc_x = (x + 0.5) / size * 2.0 - 1.0;
    // v 向下：渲染出来的图第一行在画面顶端。
    let ndc_y = 1.0 - (y + 0.5) / size * 2.0;
    (forward + right * ndc_x + up * ndc_y).normalize()
}

/// 某个方向穿出的是哪一面。
fn face_of(direction: Vec3) -> usize {
    let mut best = 0usize;
    let mut best_dot = f32::NEG_INFINITY;
    for (index, (forward, _)) in FACES.iter().enumerate() {
        let dot = direction.dot(*forward);
        if dot > best_dot {
            best_dot = dot;
            best = index;
        }
    }
    best
}

/// 方向 → 这一面上的连续纹素坐标（尚未取整）。
fn face_coords(index: usize, direction: Vec3, size: usize) -> (f32, f32) {
    let (forward, right, up) = face_basis(index);
    // 90° 视场角下 tan(半角) = 1，除以轴向分量就直接是 NDC。
    let axial = direction.dot(forward).max(1e-6);
    let ndc_x = direction.dot(right) / axial;
    let ndc_y = direction.dot(up) / axial;
    let size = size as f32;
    (
        (ndc_x * 0.5 + 0.5) * size - 0.5,
        (0.5 - ndc_y * 0.5) * size - 0.5,
    )
}

/// 取一个纹素；越出这一面时到相邻面里去取。
///
/// 越界时用**最近邻**而不是再来一次双线性：那会递归下去，而这一格
/// 本来就只占最终结果的四分之一以下。要紧的是它取的是**隔壁面真实的
/// 内容**而不是本面边缘的复制品——后者才是那道脊的来源。
fn fetch(faces: &[Face; 6], index: usize, x: i32, y: i32) -> Vec3 {
    let size = faces[index].size;
    if x >= 0 && y >= 0 && (x as usize) < size && (y as usize) < size {
        return faces[index].texel(x as usize, y as usize);
    }
    let direction = texel_direction(index, x as f32, y as f32, size);
    let neighbour = face_of(direction);
    let (nx, ny) = face_coords(neighbour, direction, faces[neighbour].size);
    let limit = faces[neighbour].size as i32 - 1;
    faces[neighbour].texel(
        nx.round().clamp(0.0, limit as f32) as usize,
        ny.round().clamp(0.0, limit as f32) as usize,
    )
}

/// 沿某个方向看过去的颜色。
///
/// 挑 `dot(方向, 面朝向)` 最大的那一面——那必然是方向穿出去的那一面——
/// 再在这一面上双线性插值。落在边缘上的取样点会自动跨到相邻面
/// （见 [`fetch`]），所以面与面之间是连续的。
pub(crate) fn sample_cube(faces: &[Face; 6], direction: Vec3) -> Vec3 {
    let direction = direction.normalize_or(Vec3::Y);
    let index = face_of(direction);
    let size = faces[index].size;
    if size == 0 {
        return Vec3::ZERO;
    }

    let (x, y) = face_coords(index, direction, size);
    let x0 = x.floor();
    let y0 = y.floor();
    let tx = x - x0;
    let ty = y - y0;
    let (x0, y0) = (x0 as i32, y0 as i32);

    let top = fetch(faces, index, x0, y0).lerp(fetch(faces, index, x0 + 1, y0), tx);
    let bottom = fetch(faces, index, x0, y0 + 1).lerp(fetch(faces, index, x0 + 1, y0 + 1), tx);
    top.lerp(bottom, ty)
}

/// 六个面 → 一张等距柱状 HDR。
///
/// 方向公式必须和 [`kpbr::hdr::HdrImage::sample_direction`] 互为逆，
/// 否则整张环境图会转一个角度——而画面上只表现为「光好像来自另一边」。
/// 这里直接用 `HdrImage::from_fn`，那一条是有测试盯着的。
pub(crate) fn cube_to_equirect(
    faces: &[Face; 6],
    width: usize,
    height: usize,
) -> kpbr::hdr::HdrImage {
    kpbr::hdr::HdrImage::from_fn(width, height, |direction| sample_cube(faces, direction))
}

/// 把回读下来的一面解成 RGB 浮点：跳掉行填充，再把半精度转成单精度。
///
/// `bytes_per_row` 是**对齐到 256 之后**的行距，不是一行像素的实际字节数。
/// `copy_texture_to_buffer` 强制这个对齐，于是每行末尾多出一段填充。
/// 按实际字节数去跳的话整张图会逐行斜着错位——而斜掉的环境图不报错，
/// 只是反射看起来「有点奇怪」。
pub(crate) fn decode_face(bytes: &[u8], size: usize, bytes_per_row: usize) -> Vec<f32> {
    let mut pixels = Vec::with_capacity(size * size * 3);
    for row in 0..size {
        let start = row * bytes_per_row;
        for column in 0..size {
            // 每像素 RGBA 四个半精度，只要前三个。
            let base = start + column * 8;
            for channel in 0..3 {
                let offset = base + channel * 2;
                pixels.push(f16_to_f32(u16::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                ])));
            }
        }
    }
    pixels
}

/// 半精度浮点 → 单精度。
///
/// HDR 目标是 `Rgba16Float`，拷回内存的是一串 `u16`。这十几行是为了
/// 不为此拖进一个 `half` 依赖——转换本身没有任何取舍可言。
pub(crate) fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;

    if exponent == 0 {
        // 次正规数（也包括 ±0）：值就是 `mantissa × 2⁻²⁴`。
        //
        // 直接乘出来而不是拼位——拼位要先把尾数重新规格化再算指数，
        // 是这整段里唯一容易差一位的地方（第一版就差了一位，
        // 把最小的次正规数解成了它的一半）。而次正规数在 HDR 里
        // 就是「几乎全黑」，差一倍也没人看得出来。
        return sign * mantissa as f32 * 2.0f32.powi(-24);
    }
    if exponent == 0x1f {
        // Inf / NaN。尾数要保留：NaN 的载荷丢了会让调试变难。
        return f32::from_bits(((bits & 0x8000) as u32) << 16 | (0xff << 23) | (mantissa << 13));
    }
    // 正规数：指数换个偏移，尾数左移补齐位宽。
    f32::from_bits(
        ((bits & 0x8000) as u32) << 16 | ((exponent + 127 - 15) << 23) | (mantissa << 13),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_faces_form_orthonormal_bases() {
        // 基不正交的话渲染和采样会用上不同的坐标系，拼出来的全景图
        // 一格一格地错位——而这不会报任何错。
        for index in 0..6 {
            let (forward, right, up) = face_basis(index);
            assert!(
                (forward.length() - 1.0).abs() < 1e-5
                    && (right.length() - 1.0).abs() < 1e-5
                    && (up.length() - 1.0).abs() < 1e-5,
                "第 {index} 面的基不是单位长度"
            );
            assert!(
                forward.dot(right).abs() < 1e-5
                    && forward.dot(up).abs() < 1e-5
                    && right.dot(up).abs() < 1e-5,
                "第 {index} 面的基不正交"
            );
            assert!(
                forward.is_finite() && right.is_finite() && up.is_finite(),
                "第 {index} 面的基里有 NaN —— 多半是朝向和上参考共线了"
            );
        }
    }

    #[test]
    fn the_face_basis_matches_the_view_matrix_used_to_render_it() {
        // 渲染那一半走 `Mat4::look_to_rh`，采样那一半走 `face_basis`。
        // 两边的右轴和上轴必须逐分量相同，否则每个面都会被水平或
        // 竖直翻转一次——拼起来的全景图看着「有内容」，只是全错。
        for index in 0..6 {
            let (_, right, up) = face_basis(index);
            let camera_to_world = face_camera_to_world(Vec3::ZERO, index);
            let matrix_right = camera_to_world.x_axis.truncate();
            let matrix_up = camera_to_world.y_axis.truncate();

            assert!(
                (right - matrix_right).length() < 1e-5,
                "第 {index} 面的右轴对不上：采样 {right:?}，视图矩阵 {matrix_right:?}"
            );
            assert!(
                (up - matrix_up).length() < 1e-5,
                "第 {index} 面的上轴对不上：采样 {up:?}，视图矩阵 {matrix_up:?}"
            );
        }
    }

    /// 把「方向 → 颜色」的函数烘成六个面，用的正是渲染时那套投影。
    fn bake(size: usize, radiance: impl Fn(Vec3) -> Vec3) -> [Face; 6] {
        std::array::from_fn(|index| {
            let (forward, right, up) = face_basis(index);
            let mut pixels = Vec::with_capacity(size * size * 3);
            for y in 0..size {
                for x in 0..size {
                    let ndc_x = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                    let ndc_y = 1.0 - (y as f32 + 0.5) / size as f32 * 2.0;
                    let direction = (forward + right * ndc_x + up * ndc_y).normalize();
                    let value = radiance(direction);
                    pixels.extend_from_slice(&[value.x, value.y, value.z]);
                }
            }
            Face { size, pixels }
        })
    }

    #[test]
    fn a_direction_survives_the_round_trip_through_the_cube() {
        // 把方向本身当颜色烘进六个面，再按方向采回来。取不回同一个
        // 方向就说明某个面的轴反了——而反了之后画面上只是
        // 「环境反射的方位不太对」，没人分得清。
        let faces = bake(64, |d| d * 0.5 + Vec3::splat(0.5));

        for direction in [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
            Vec3::new(0.4, 0.6, -0.7).normalize(),
            Vec3::new(-0.9, -0.2, 0.35).normalize(),
        ] {
            let expected = direction * 0.5 + Vec3::splat(0.5);
            let actual = sample_cube(&faces, direction);
            assert!(
                (actual - expected).length() < 0.02,
                "方向 {direction:?} 取回来是 {actual:?}，该是 {expected:?}"
            );
        }
    }

    #[test]
    fn the_equirect_keeps_the_directions_it_was_given() {
        // 立方图 → 等距柱状 → 按方向采样，绕一整圈还要认得出方向。
        // 中间任何一步的坐标系错了，整张环境图就转了一个角度。
        let faces = bake(64, |d| d * 0.5 + Vec3::splat(0.5));
        let image = cube_to_equirect(&faces, 128, 64);

        for direction in [
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::X,
            Vec3::NEG_Z,
            Vec3::new(0.3, 0.4, 0.87).normalize(),
        ] {
            let expected = direction * 0.5 + Vec3::splat(0.5);
            let actual = image.sample_direction(direction);
            assert!(
                (actual - expected).length() < 0.06,
                "方向 {direction:?} 绕一圈回来是 {actual:?}，该是 {expected:?}"
            );
        }
    }

    /// 一个方向上变化明显、但处处平滑的辐射亮度函数。
    ///
    /// 常量场测不出接缝（两边一样），所以要一个有梯度的。
    fn smooth(d: Vec3) -> Vec3 {
        Vec3::new(
            0.5 + 0.5 * (d.x * 2.0).sin(),
            0.5 + 0.5 * (d.y * 2.0 + 1.0).sin(),
            0.5 + 0.5 * (d.z * 2.0 + 2.0).sin(),
        )
    }

    /// 一批**横跨立方体棱**的方向对：两个方向只差一点点，但分属两个面。
    fn across_the_seams() -> Vec<(Vec3, Vec3)> {
        // 沿 +X 面和 +Y 面之间那条棱扫一圈，再取 +X / +Z 那条。
        let mut pairs = Vec::new();
        for step in 0..40 {
            let t = -0.9 + step as f32 * 0.045;
            for (a, b, along) in [
                (Vec3::X, Vec3::Y, Vec3::Z),
                (Vec3::X, Vec3::Z, Vec3::Y),
                (Vec3::NEG_Y, Vec3::Z, Vec3::X),
            ] {
                // 棱上的方向，再往两边各偏一丁点。
                let edge = (a + b).normalize() + along * t;
                let tangent = (b - a).normalize();
                let epsilon = 0.004;
                pairs.push((
                    (edge - tangent * epsilon).normalize(),
                    (edge + tangent * epsilon).normalize(),
                ));
            }
        }
        pairs
    }

    #[test]
    fn there_is_no_ridge_where_two_faces_meet() {
        // 双线性要取四个相邻纹素，而落在面边缘上的那些有一半在隔壁面里。
        // 夹到边上的话两个面各自把最外圈往外抹一格，交界处留下一道
        // 一格宽的脊——在低粗糙度的镜面反射上看得见，而且没有任何报错。
        //
        // 这里验的是**连续性**：只差 0.008 弧度的两个方向，取回来的颜色
        // 也该只差一点点。不需要解析参照，所以这条测的就是接缝本身。
        let faces = bake(32, smooth);

        let mut worst = 0.0f32;
        let mut worst_at = Vec3::ZERO;
        for (a, b) in across_the_seams() {
            let jump = (sample_cube(&faces, a) - sample_cube(&faces, b)).length();
            if jump > worst {
                worst = jump;
                worst_at = a;
            }
        }
        assert!(
            worst < 0.02,
            "跨棱的两个相邻方向差了 {worst:.4}（在 {worst_at:?} 附近）——接缝上有一道脊"
        );
    }

    #[test]
    fn the_seam_samples_stay_close_to_the_function_they_came_from() {
        // 连续不等于正确：两边都取成同一个错值也是连续的。
        // 所以再拿解析值对一遍，确认跨面取到的是**隔壁面真实的内容**。
        //
        // 说清楚这条测的不是接缝：夹边的老做法在 32² 下也能过这一条
        // （误差 0.02 出头，还在容差里）。真正抓接缝的是上面那条连续性，
        // 换回夹边它会红。这一条守的是「跨面之后取的值仍然对」。
        let faces = bake(32, smooth);
        let mut worst = 0.0f32;
        for (a, b) in across_the_seams() {
            for direction in [a, b] {
                let error = (sample_cube(&faces, direction) - smooth(direction)).length();
                worst = worst.max(error);
            }
        }
        assert!(worst < 0.025, "接缝附近和解析值最大差 {worst:.4}");
    }

    #[test]
    fn a_uniform_cube_stays_uniform() {
        // 接缝处如果取错了面，会在全景图上留下一圈亮度不同的条纹。
        // 常量输入下任何取样错误都不该改变结果。
        let faces = bake(32, |_| Vec3::new(0.25, 0.5, 0.75));
        let image = cube_to_equirect(&faces, 96, 48);
        for chunk in image.pixels().chunks_exact(3) {
            assert!(
                (chunk[0] - 0.25).abs() < 1e-4
                    && (chunk[1] - 0.5).abs() < 1e-4
                    && (chunk[2] - 0.75).abs() < 1e-4,
                "常量环境里出现了 {chunk:?}"
            );
        }
    }

    #[test]
    fn the_row_padding_is_skipped_when_decoding() {
        // `copy_texture_to_buffer` 要求行距对齐到 256，于是每行末尾
        // 多出一段填充。按一行像素的实际字节数去跳的话，整张图会
        // 逐行斜着错位——而斜掉的环境图不报错，只是「看起来有点奇怪」。
        let size = 3usize;
        // 一行实际 3 × 8 = 24 字节，对齐后是 256。
        let bytes_per_row = 256usize;
        let mut bytes = vec![0u8; bytes_per_row * size];
        for row in 0..size {
            for column in 0..size {
                let base = row * bytes_per_row + column * 8;
                // 红通道存 1.0，绿存 2.0，蓝存 0.5，alpha 随便。
                bytes[base..base + 2].copy_from_slice(&0x3c00u16.to_le_bytes());
                bytes[base + 2..base + 4].copy_from_slice(&0x4000u16.to_le_bytes());
                bytes[base + 4..base + 6].copy_from_slice(&0x3800u16.to_le_bytes());
            }
            // 填充区塞上垃圾。跳错了就会读到它。
            let padding = row * bytes_per_row + size * 8;
            bytes[padding..(row + 1) * bytes_per_row].fill(0xAB);
        }

        let pixels = decode_face(&bytes, size, bytes_per_row);
        assert_eq!(pixels.len(), size * size * 3);
        for (index, chunk) in pixels.chunks_exact(3).enumerate() {
            assert_eq!(
                chunk,
                [1.0, 2.0, 0.5],
                "第 {index} 个像素读到了填充区的垃圾：{chunk:?}"
            );
        }
    }

    #[test]
    fn half_floats_decode_to_the_values_they_encode() {
        // 解码错了整张环境图的亮度就是错的，而 HDR 本来就没有
        // 「看起来该多亮」的直觉参照——只会表现为环境光偏了。
        for (bits, expected) in [
            (0x0000u16, 0.0f32),
            (0x8000, -0.0),
            (0x3c00, 1.0),
            (0xbc00, -1.0),
            (0x4000, 2.0),
            (0x3800, 0.5),
            (0x7bff, 65504.0),
            (0x3555, 0.333_251_95),
            // 次正规数：最小的那个。
            (0x0001, 5.960_464_5e-8),
        ] {
            let actual = f16_to_f32(bits);
            assert!(
                (actual - expected).abs() <= expected.abs() * 1e-5 + 1e-12,
                "0x{bits:04x} 解成了 {actual}，该是 {expected}"
            );
        }
        assert!(f16_to_f32(0x7c00).is_infinite());
        assert!(f16_to_f32(0x7e00).is_nan());
    }
}
