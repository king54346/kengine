//! 例子级截图回归：真把例子跑起来（窗口、插件、脚本、资源加载都在），截第 N 帧比对。
//!
//! ```bash
//! cargo build --examples                                              # 先编好例子
//! cargo test --test example_screenshots -- --ignored                  # 比对
//! KENGINE_BLESS=1 cargo test --test example_screenshots -- --ignored  # 刷新基准图
//! ```
//!
//! 和 `screenshots.rs`（无头渲染器画的小场景）互补：那边管渲染器的单项功能，
//! 这边管「整条链路接起来以后画面还对不对」——插件顺序、脚本、资源异步加载、UI。
//!
//! 能比，靠的是 `KENGINE_FIXED_DT=1/60`：动画、物理、脚本、着色器时间都按固定步长走，
//! 第 90 帧的画面和机器快慢无关。实测同一例子连跑两次，最多一两个像素差一级。
//!
//! 默认 `#[ignore]`，理由同 `screenshots.rs`；另外要开窗口，没有显示器的机器跑不了。
//! 没编过的例子跳过并提示，不算失败。

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// 截哪几个、第几帧。挑的是覆盖面：物理、各类光源、UI、材质、glTF、后处理、脚本。
const EXAMPLES: &[(&str, u32)] = &[
    ("physics_character", 90),
    ("physics_terrain", 90),
    ("lights_spotlight", 90),
    ("lights_custom", 90),
    ("lights_many", 90),
    ("lights_rectarealight", 90),
    ("first_person_view_model", 90),
    ("ui_widgets", 30),
    ("materials_transmission", 90),
    ("materials_toon", 90),
    ("loader_gltf", 120),
    ("gltf_skinned_mesh", 90),
    ("postprocessing_bloom", 90),
    ("postprocessing_ssr", 90),
    ("postprocessing_ao", 90),
    ("demo", 120),
    ("ocean", 120),
    ("water_island", 240),
    ("localization", 30),
];

/// 截图缩到这个宽度：够看出东西对不对，基准图也小。
const WIDTH: u32 = 480;

fn example_exe(name: &str) -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(profile)
        .join("examples")
        .join(exe)
}

#[test]
#[ignore]
fn examples_look_the_same() {
    let shots = common::output_dir().join("examples-raw");
    std::fs::create_dir_all(&shots).unwrap();
    let mut failures = Vec::new();
    let mut skipped = Vec::new();
    for &(name, frame) in EXAMPLES {
        let exe = example_exe(name);
        if !exe.exists() {
            skipped.push(name);
            continue;
        }
        let shot = shots.join(format!("{name}.png"));
        let _ = std::fs::remove_file(&shot);
        let status = Command::new(&exe)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("KENGINE_FIXED_DT", "1/60")
            // 脚本里的 `Math.random` 也要可复现（demo 的刷怪位置是随机的）。
            .env("KENGINE_SEED", "1")
            .env("KENGINE_SCREENSHOT", &shot)
            .env("KENGINE_SCREENSHOT_FRAME", frame.to_string())
            .env("KENGINE_SCREENSHOT_EXIT", "1")
            .env("KENGINE_SCREENSHOT_WIDTH", WIDTH.to_string())
            .env("KENGINE_PRESENT", "immediate")
            // 例子的日志收起来，只在出事时附上最后几行。
            .output();
        let image = match status {
            Ok(_) if shot.exists() => std::fs::read(&shot).unwrap(),
            Ok(output) => {
                let log = String::from_utf8_lossy(&output.stderr);
                let tail: Vec<&str> = log.lines().rev().take(12).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                failures.push(format!(
                    "{name}：没截到图（{}）\n{}",
                    output.status,
                    tail.join("\n")
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{name}：起不来：{error}"));
                continue;
            }
        };
        let texture = kengine::ktexture::Texture::from_encoded(&image).unwrap();
        if let Err(message) = common::verify(
            &format!("examples/{name}"),
            texture.width(),
            texture.height(),
            texture.data(),
        ) {
            failures.push(message);
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "没编过、跳过了：{}（先 cargo build --examples）",
            skipped.join("、")
        );
    }
    assert!(
        failures.is_empty(),
        "{} 个例子和基准图不一致：\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
