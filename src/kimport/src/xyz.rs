//! XYZ 点云：每行一个点，`x y z` 或 `x y z r g b`。
//!
//! 没有正式规范的「格式」，扫描仪和化学软件各写各的。这里按 three.js
//! `XYZLoader` 的口径读：
//!
//! - `#` 开头的行是注释，空行跳过；
//! - 分隔符是任意空白或逗号（CSV 导出的 XYZ 很常见）；
//! - **所有**点都是 6 列时才算带颜色，颜色是 0–255 的 sRGB，转成线性存；
//! - 少于 3 个数的行跳过，多出来的列（强度、法线）忽略。
//!
//! 产物复用 [`PointCloud`]——点云画法（[`PointCloud::to_mesh`]）不因为
//! 文件格式不同而不同。
//!
//! # 不支持
//!
//! 化学领域的 `.xyz`（首行原子数、次行注释、之后是 `元素 x y z`）不是这个
//! 格式；元素符号那一列会让这一行被当成「少于 3 个数」跳过。

use crate::{bad, limits, loader, pcd::POINT_CLOUD_TYPE_UUID, pcd::PointCloud};
use kasset::{LoadError, ResourceIo};
use kmath::Vec3;
use std::{path::PathBuf, sync::Arc};

loader! {
    /// 读 `.xyz`。产物是 [`PointCloud`]，和 PCD 同一个资源类型。
    XyzLoader -> PointCloud : ["xyz"] = POINT_CLOUD_TYPE_UUID, load
}

/// [`loader!`] 要的异步签名。
pub async fn load(
    bytes: Vec<u8>,
    _path: PathBuf,
    _io: Arc<dyn ResourceIo>,
) -> Result<PointCloud, LoadError> {
    parse(&bytes)
}

/// sRGB（0–1）→ 线性。和 three.js `SRGBColorSpace` 的转换相同。
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 解析 XYZ 文本。
pub fn parse(bytes: &[u8]) -> Result<PointCloud, LoadError> {
    let text = String::from_utf8_lossy(bytes);
    let mut positions = Vec::new();
    let mut colors = Vec::new();
    let mut all_colored = true;
    let mut values = Vec::with_capacity(6);

    for (number, line) in text.lines().enumerate() {
        if number >= limits::LINES {
            return Err(bad("XYZ 行数超过上限"));
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        values.clear();
        values.extend(
            line.split(|c: char| c.is_whitespace() || c == ',')
                .filter(|token| !token.is_empty())
                .take(6)
                .map_while(|token| token.parse::<f32>().ok()),
        );
        if values.len() < 3 {
            continue;
        }
        if positions.len() >= limits::VERTICES {
            return Err(bad("XYZ 点数超过上限"));
        }
        positions.push(Vec3::new(values[0], values[1], values[2]));
        if values.len() >= 6 {
            colors.push(Vec3::new(
                srgb_to_linear((values[3] / 255.0).clamp(0.0, 1.0)),
                srgb_to_linear((values[4] / 255.0).clamp(0.0, 1.0)),
                srgb_to_linear((values[5] / 255.0).clamp(0.0, 1.0)),
            ));
        } else {
            all_colored = false;
            colors.push(Vec3::ONE);
        }
    }

    if positions.is_empty() {
        return Err(bad("XYZ 里没有点"));
    }
    // 只有一部分点带颜色时整体当成无色——半白半彩的点云比统一着色更难看，
    // 调用方拿到 `has_color = false` 会自己按高度之类的规则上色。
    if !all_colored {
        colors.fill(Vec3::ONE);
    }
    Ok(PointCloud {
        positions,
        colors,
        has_color: all_colored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_positions_and_skips_comments() {
        let cloud = parse(b"# helix\n#\n  1 2 3\n\n4.5 -5 6e-1\n").unwrap();
        assert_eq!(cloud.positions, vec![Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.5, -5.0, 0.6)]);
        assert!(!cloud.has_color);
    }

    #[test]
    fn six_columns_carry_srgb_colours() {
        let cloud = parse(b"0 0 0 255 0 128\n1,1,1,0,255,0").unwrap();
        assert!(cloud.has_color);
        assert_eq!(cloud.colors[0].x, 1.0);
        // 128/255 的 sRGB 约等于 0.216 的线性值。
        assert!((cloud.colors[0].z - 0.2158).abs() < 1e-3);
        assert_eq!(cloud.colors[1], Vec3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn mixed_rows_fall_back_to_uncoloured() {
        let cloud = parse(b"0 0 0 255 0 0\n1 1 1\n").unwrap();
        assert!(!cloud.has_color);
        assert!(cloud.colors.iter().all(|&c| c == Vec3::ONE));
    }

    #[test]
    fn junk_lines_are_skipped_and_empty_files_rejected() {
        let cloud = parse(b"C 0 0 0\n1 2\n7 8 9 intensity\n").unwrap();
        assert_eq!(cloud.positions, vec![Vec3::new(7.0, 8.0, 9.0)]);
        assert!(parse(b"# nothing\n").is_err());
    }
}
