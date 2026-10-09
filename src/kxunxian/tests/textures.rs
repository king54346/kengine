//! 用引擎的 DDS 解码器把 zj 衣柜、武器、法宝材质表引用的每张贴图都解一遍（`assets/unpack2` 不在就跳过）。
//! 文件本身缺失的（源数据里就没有）只计数，不算失败；文件在但解不了的才算。

use std::path::PathBuf;

use kxunxian::config::parse_materials;

#[test]
fn every_referenced_texture_decodes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/unpack2");
    if !root.join("cha/share/config").is_dir() {
        return;
    }
    let library = kxunxian::Library::new(&root);
    let mut missing = 0;
    let mut failed = Vec::new();
    let mut decoded = 0;
    for table in [
        "zj_nvxing_001.cmf",
        "zj_nanxing_001.cmf",
        "wq_wq_001.cmf",
        "fb_fb_001.cmf",
    ] {
        let bytes = std::fs::read(root.join("cha/share/config").join(table)).unwrap();
        for (name, def) in parse_materials(&bytes) {
            let Some(path) = library.resolve(&def.base_map) else {
                missing += 1;
                continue;
            };
            match ktexture::Texture::from_encoded(&std::fs::read(&path).unwrap()) {
                Ok(_) => decoded += 1,
                Err(error) => failed.push(format!("{name}: {} — {error}", path.display())),
            }
        }
    }
    println!("解出 {decoded} 张，缺文件 {missing} 张");
    assert!(
        failed.is_empty(),
        "{} 张解不了：\n{}",
        failed.len(),
        failed.join("\n")
    );
}
