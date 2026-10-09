//! 截图回归两组测试共用的比对：8×8 分块平均色 + 全图平均差，基准图在 `tests/golden/`。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// 单块平均色允许差多少（0–255，取三个通道里最大的）。
pub const BLOCK_LIMIT: f32 = 14.0;
/// 全图平均差允许多少。
pub const MEAN_LIMIT: f32 = 2.5;

pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

pub fn output_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots")
}

/// 和 `tests/golden/{name}.png` 比。没有基准图或设了 `KENGINE_BLESS` 时写入基准图。
/// 不一致时把实际图和差异图写到 `target/screenshots/`，返回说明。
pub fn verify(name: &str, width: u32, height: u32, actual: &[u8]) -> Result<(), String> {
    let golden_path = golden_dir().join(format!("{name}.png"));
    if std::env::var_os("KENGINE_BLESS").is_some() || !golden_path.exists() {
        std::fs::create_dir_all(golden_path.parent().unwrap()).unwrap();
        kengine::ktexture::write_png(&golden_path, width, height, actual).unwrap();
        eprintln!("写入基准图 {}", golden_path.display());
        return Ok(());
    }

    let golden =
        kengine::ktexture::Texture::from_encoded(&std::fs::read(&golden_path).unwrap()).unwrap();
    if (golden.width(), golden.height()) != (width, height) {
        return Err(format!(
            "{name}：基准图 {}×{}，实际 {width}×{height}，刷新一下",
            golden.width(),
            golden.height()
        ));
    }
    let (worst, worst_at, mean) = compare(golden.data(), actual, width, height);
    if worst > BLOCK_LIMIT || mean > MEAN_LIMIT {
        let actual_path = output_dir().join(format!("{name}.actual.png"));
        let diff_path = output_dir().join(format!("{name}.diff.png"));
        std::fs::create_dir_all(actual_path.parent().unwrap()).unwrap();
        kengine::ktexture::write_png(&actual_path, width, height, actual).unwrap();
        let diff: Vec<u8> = golden
            .data()
            .chunks_exact(4)
            .zip(actual.chunks_exact(4))
            .flat_map(|(a, b)| {
                let d = |i: usize| (a[i].abs_diff(b[i]) as u32 * 8).min(255) as u8;
                [d(0), d(1), d(2), 255]
            })
            .collect();
        kengine::ktexture::write_png(&diff_path, width, height, &diff).unwrap();
        return Err(format!(
            "{name}：和基准图不一致——最差的一块差 {worst:.1}（在 {worst_at:?}，上限 {BLOCK_LIMIT}），\
             平均差 {mean:.2}（上限 {MEAN_LIMIT}）。\n实际：{}\n差异：{}\n确认是有意的改动就 KENGINE_BLESS=1 重跑。",
            actual_path.display(),
            diff_path.display()
        ));
    }
    Ok(())
}

/// 返回（最差那块的差, 那块的像素坐标, 全图平均差）。
pub fn compare(a: &[u8], b: &[u8], width: u32, height: u32) -> (f32, (u32, u32), f32) {
    const BLOCK: u32 = 8;
    let mut worst = 0.0f32;
    let mut worst_at = (0, 0);
    let mut total = 0.0f64;
    for by in (0..height).step_by(BLOCK as usize) {
        for bx in (0..width).step_by(BLOCK as usize) {
            let mut sum = [0.0f32; 3];
            let mut count = 0.0;
            for y in by..(by + BLOCK).min(height) {
                for x in bx..(bx + BLOCK).min(width) {
                    let i = ((y * width + x) * 4) as usize;
                    for c in 0..3 {
                        let d = f32::from(a[i + c]) - f32::from(b[i + c]);
                        sum[c] += d;
                        total += f64::from(d.abs());
                    }
                    count += 1.0;
                }
            }
            let block = sum.iter().map(|s| (s / count).abs()).fold(0.0, f32::max);
            if block > worst {
                worst = block;
                worst_at = (bx, by);
            }
        }
    }
    (
        worst,
        worst_at,
        (total / f64::from(width * height * 3)) as f32,
    )
}
