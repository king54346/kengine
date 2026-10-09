//! three.js 仓库里所有的 SVG 样例：都要能解析（不崩、不死循环），
//! 大部分要能画出东西来。

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("svg") {
            out.push(path);
        }
    }
}

#[test]
fn every_sample_svg_parses_and_tessellates() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/threejs/models/svg");
    let mut files = Vec::new();
    collect(&dir, &mut files);
    if files.is_empty() {
        eprintln!("没有样例目录 {}，跳过", dir.display());
        return;
    }
    let mut drawn = 0;
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let document =
            kimport::svg::parse(&bytes, 0.25).unwrap_or_else(|e| panic!("{}：{e}", file.display()));
        let triangles: usize = document
            .paths
            .iter()
            .map(|p| {
                p.fill_tessellation().triangle_count() * p.fill.is_some() as usize
                    + p.stroke_tessellation().triangle_count()
            })
            .sum();
        eprintln!(
            "{}：{} 条路径，{triangles} 个三角形",
            file.file_name().unwrap().to_string_lossy(),
            document.paths.len()
        );
        if triangles > 0 {
            drawn += 1;
        }
    }
    // emptyPath.svg 之类本来就是空的；其余都该画出东西。
    assert!(
        drawn * 10 >= files.len() * 8,
        "只有 {drawn}/{} 个文件画出了东西",
        files.len()
    );
}
