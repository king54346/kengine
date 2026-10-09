//! three.js 仓库里那 20 份 MaterialX 样例：每份都要能导入，生成的钩子
//! 都要能和引擎的标准着色器拼起来通过校验。
//!
//! 着色器编不过在运行时只表现为「这个材质退回了标准管线」，肉眼很难
//! 分辨，所以在这里钉住。

use std::path::Path;

const SAMPLES: &str = "../../examples/threejs/materialx";

#[test]
fn every_local_sample_imports_and_its_hook_validates() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(SAMPLES);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("没有样例目录 {}，跳过", dir.display());
        return;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mtlx") {
            continue;
        }
        let materials = kimport::materialx::load(&path)
            .unwrap_or_else(|e| panic!("{} 导入失败：{e}", path.display()));
        assert!(
            !materials.is_empty(),
            "{} 里一个材质都没导出来",
            path.display()
        );
        for material in materials {
            if let Some(source) = &material.shader_source
                && let Err(error) = krender::validate_material_hook(source)
            {
                panic!(
                    "{} / {} 的钩子过不了校验：{error}\n{source}",
                    path.display(),
                    material.name
                );
            }
            count += 1;
        }
    }
    assert!(count >= 20, "只导出了 {count} 个材质");
}
