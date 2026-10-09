//! 每个 zj 身体把整座衣柜（所有部件）拼一遍，数还有哪些部件拿不到贴图（画出来就是白的）。
//! `assets/unpack2` 不在就跳过。源数据里确实对不上的几个（材质表里压根没有）列在 `KNOWN_BROKEN`。

use std::path::PathBuf;

use kxunxian::{Library, Outfit};

/// 源数据里怎么都对不上的：
/// - `m_mz554_mz_554` → `m_mz546mz_554`（编号 546 / 554 自相矛盾）；
/// - `m_hdsp_hd_001` → `zj_zjhdsp_*`（材质表里没有这一族）；
/// - `c_yf729_pf_004` → `m_yf729pf_004_h`（任何一张表里都没有 yf729 的披风）；
/// - `m_abccj00` → 材质名就叫 `noname`（选人界面用的 xuanren 身体）。
const KNOWN_BROKEN: [&str; 4] = [
    "m_mz554_mz_554",
    "m_hdsp_hd_001",
    "c_yf729_pf_004",
    "m_abccj00",
];

#[test]
fn every_wardrobe_part_gets_a_texture() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/unpack2");
    let special = root.join("cha/special");
    if !special.is_dir() {
        return;
    }
    let mut library = Library::new(&root);
    let mut untextured = Vec::new();
    let mut bodies: Vec<_> = std::fs::read_dir(&special)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("zj_"))
        .collect();
    bodies.sort();
    for body in bodies {
        let Ok(character) = library.character(&body) else {
            continue;
        };
        let parts: Vec<String> = character
            .def
            .models
            .iter()
            .filter(|m| !m.material.is_empty())
            .map(|m| m.name.clone())
            .collect();
        let model = library.build(
            &character,
            &Outfit {
                parts,
                equips: Vec::new(),
            },
        );
        for node in model.nodes() {
            for part in &node.parts {
                let textured = part
                    .material
                    .and_then(|index| model.material(index))
                    .is_some_and(|material| material.base_color_texture().is_some());
                if !textured && !KNOWN_BROKEN.contains(&node.name.to_lowercase().as_str()) {
                    untextured.push(format!("{body}: {}", node.name));
                }
            }
        }
    }
    assert!(
        untextured.is_empty(),
        "{} 个部件没贴图：\n{}",
        untextured.len(),
        untextured.join("\n")
    );
}
