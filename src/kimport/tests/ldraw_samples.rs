//! three.js 仓库里那 17 个官方 Packed 模型：每个都要能完整展开，
//! 一个零件都不缺（Packed 格式把用到的零件全部内联了）。

use std::path::Path;

const MODELS: &str = "../../examples/threejs/models/ldraw/officialLibrary/models";

#[test]
fn every_packed_model_expands_without_missing_parts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODELS);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("没有样例目录 {}，跳过", dir.display());
        return;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let started = std::time::Instant::now();
        let parsed = kengine_ldraw(&path);
        let triangles: usize = parsed
            .model
            .meshes()
            .iter()
            .map(|m| m.indices().len() / 3)
            .sum();
        eprintln!(
            "{}：{triangles} 个三角形，{} 步，{:.0} ms",
            path.file_name().unwrap().to_string_lossy(),
            parsed.steps,
            started.elapsed().as_secs_f32() * 1000.0
        );
        assert!(
            parsed.missing.is_empty(),
            "{} 缺零件：{:?}",
            path.display(),
            parsed.missing
        );
        assert!(triangles > 100, "{} 几乎是空的", path.display());
        count += 1;
    }
    assert_eq!(count, 17);
}

fn kengine_ldraw(path: &Path) -> kimport::ldraw::LDrawModel {
    kimport::ldraw::load(path).unwrap_or_else(|e| panic!("{} 导入失败：{e}", path.display()))
}
