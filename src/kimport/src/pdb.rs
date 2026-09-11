//! PDB（Protein Data Bank）。原子坐标与 `CONECT` 给出的化学键。
//!
//! # 支持
//!
//! `ATOM` / `HETATM` 的坐标与元素符号，`CONECT` 的成键关系。
//! 元素符号优先取第 77–78 列（规范位置），那一列为空时退回第 13–14 列
//! 的原子名——样本里一半的文件只有 66 个字符宽，根本没有第 77 列。
//!
//! # 不支持
//!
//! 二级结构（`HELIX` / `SHEET`）、多模型（`MODEL` / `ENDMDL`，只当成
//! 一个模型连着读）、对称操作（`CRYST1` / `MTRIX`）、以及从距离推断
//! 化学键——没有 `CONECT` 的文件就没有键，和 three.js 的 `PDBLoader` 一致。
//!
//! # 颜色是 CPK 配色
//!
//! 元素 → 颜色的表直接用化学界通行的 CPK/Jmol 配色（碳灰、氮蓝、氧红、
//! 硫黄），和 three.js `PDBLoader` 里那张表同源，这样两边的截图能对着看。
//! 表里没有的元素退回品红——一个刺眼的颜色比一个看起来合理的默认色好，
//! 至少一眼能看出「这个元素没认出来」。

use crate::{bad, limits};
use kasset::LoadError;
use kmath::Vec3;

/// 一个原子。
#[derive(Debug, Clone, Copy)]
pub struct Atom {
    /// 坐标，单位埃。
    pub position: Vec3,
    /// CPK 配色。
    pub color: Vec3,
    /// 元素序号在 [`ELEMENTS`] 里的下标，未识别时为 `None`。
    pub element: Option<usize>,
}

/// 一根化学键：两个原子在 [`Molecule::atoms`] 里的下标。
pub type Bond = (usize, usize);

/// 一个分子。
#[derive(Debug, Clone, Default)]
pub struct Molecule {
    /// 所有原子。
    pub atoms: Vec<Atom>,
    /// 所有化学键，同一对原子只出现一次。
    pub bonds: Vec<Bond>,
}

impl Molecule {
    /// 几何中心，摆相机用。
    pub fn center(&self) -> Vec3 {
        if self.atoms.is_empty() {
            return Vec3::ZERO;
        }
        self.atoms.iter().map(|a| a.position).sum::<Vec3>() / self.atoms.len() as f32
    }

    /// 到几何中心的最大距离，决定相机该退多远。
    pub fn radius(&self) -> f32 {
        let center = self.center();
        self.atoms
            .iter()
            .map(|a| a.position.distance(center))
            .fold(0.0, f32::max)
    }

    /// 元素符号，未识别时返回 `"?"`。
    pub fn symbol(&self, atom: &Atom) -> &'static str {
        atom.element.map_or("?", |index| ELEMENTS[index].0)
    }
}

/// 元素符号（小写）到 CPK 颜色的对照表。
pub const ELEMENTS: &[(&str, [u8; 3])] = &[
    ("h",[255,255,255]), ("he",[217,255,255]), ("li",[204,128,255]), ("be",[194,255,0]),
    ("b",[255,181,181]), ("c",[144,144,144]), ("n",[48,80,248]), ("o",[255,13,13]),
    ("f",[144,224,80]), ("ne",[179,227,245]), ("na",[171,92,242]), ("mg",[138,255,0]),
    ("al",[191,166,166]), ("si",[240,200,160]), ("p",[255,128,0]), ("s",[255,255,48]),
    ("cl",[31,240,31]), ("ar",[128,209,227]), ("k",[143,64,212]), ("ca",[61,255,0]),
    ("sc",[230,230,230]), ("ti",[191,194,199]), ("v",[166,166,171]), ("cr",[138,153,199]),
    ("mn",[156,122,199]), ("fe",[224,102,51]), ("co",[240,144,160]), ("ni",[80,208,80]),
    ("cu",[200,128,51]), ("zn",[125,128,176]), ("ga",[194,143,143]), ("ge",[102,143,143]),
    ("as",[189,128,227]), ("se",[255,161,0]), ("br",[166,41,41]), ("kr",[92,184,209]),
    ("rb",[112,46,176]), ("sr",[0,255,0]), ("y",[148,255,255]), ("zr",[148,224,224]),
    ("nb",[115,194,201]), ("mo",[84,181,181]), ("tc",[59,158,158]), ("ru",[36,143,143]),
    ("rh",[10,125,140]), ("pd",[0,105,133]), ("ag",[192,192,192]), ("cd",[255,217,143]),
    ("in",[166,117,115]), ("sn",[102,128,128]), ("sb",[158,99,181]), ("te",[212,122,0]),
    ("i",[148,0,148]), ("xe",[66,158,176]), ("cs",[87,23,143]), ("ba",[0,201,0]),
    ("la",[112,212,255]), ("ce",[255,255,199]), ("pr",[217,255,199]), ("nd",[199,255,199]),
    ("pm",[163,255,199]), ("sm",[143,255,199]), ("eu",[97,255,199]), ("gd",[69,255,199]),
    ("tb",[48,255,199]), ("dy",[31,255,199]), ("ho",[0,255,156]), ("er",[0,230,117]),
    ("tm",[0,212,82]), ("yb",[0,191,56]), ("lu",[0,171,36]), ("hf",[77,194,255]),
    ("ta",[77,166,255]), ("w",[33,148,214]), ("re",[38,125,171]), ("os",[38,102,150]),
    ("ir",[23,84,135]), ("pt",[208,208,224]), ("au",[255,209,35]), ("hg",[184,184,208]),
    ("tl",[166,84,77]), ("pb",[87,89,97]), ("bi",[158,79,181]), ("po",[171,92,0]),
    ("at",[117,79,69]), ("rn",[66,130,150]), ("fr",[66,0,102]), ("ra",[0,125,0]),
    ("ac",[112,171,250]), ("th",[0,186,255]), ("pa",[0,161,255]), ("u",[0,143,255]),
    ("np",[0,128,255]), ("pu",[0,107,255]), ("am",[84,92,242]), ("cm",[120,92,227]),
    ("bk",[138,79,227]), ("cf",[161,54,212]), ("es",[179,31,212]), ("fm",[179,31,186]),
    ("md",[179,13,166]), ("no",[189,13,135]), ("lr",[199,0,102]), ("rf",[204,0,89]),
    ("db",[209,0,79]), ("sg",[217,0,69]), ("bh",[224,0,56]), ("hs",[230,0,46]),
    ("mt",[235,0,38]), ("ds",[235,0,38]), ("rg",[235,0,38]), ("cn",[235,0,38]),
    ("uut",[235,0,38]), ("uuq",[235,0,38]), ("uup",[235,0,38]), ("uuh",[235,0,38]),
    ("uus",[235,0,38]), ("uuo",[235,0,38])
];

/// 没认出来的元素用品红，见模块文档。
const UNKNOWN: Vec3 = Vec3::new(1.0, 0.0, 1.0);

/// 解析 PDB 文本。
pub fn parse(bytes: &[u8]) -> Result<Molecule, LoadError> {
    let text = String::from_utf8_lossy(bytes);
    let mut molecule = Molecule::default();
    // PDB 的原子序号（第 7–11 列）不保证从 1 连续排，`CONECT` 引用的是
    // 那个序号而不是行号，所以要单独记一张序号 → 下标的表。
    let mut by_serial: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    for (number, line) in text.lines().enumerate() {
        if number > limits::LINES {
            return Err(bad("PDB 行数超过上限"));
        }
        if line.starts_with("ATOM") || line.starts_with("HETATM") {
            let Some(position) = columns_vec3(line) else {
                continue;
            };
            let symbol = element_symbol(line);
            let element = ELEMENTS.iter().position(|(name, _)| *name == symbol);
            let color = element.map_or(UNKNOWN, |index| {
                let [r, g, b] = ELEMENTS[index].1;
                Vec3::new(r as f32, g as f32, b as f32) / 255.0
            });
            if let Some(serial) = column_u32(line, 6, 11) {
                by_serial.insert(serial, molecule.atoms.len());
            }
            molecule.atoms.push(Atom {
                position,
                color,
                element,
            });
            if molecule.atoms.len() > limits::VERTICES / 64 {
                return Err(bad("PDB 原子数超过上限"));
            }
        } else if line.starts_with("CONECT") {
            let Some(from) = column_u32(line, 6, 11).and_then(|s| by_serial.get(&s).copied()) else {
                continue;
            };
            for start in [11, 16, 21, 26] {
                let Some(to) = column_u32(line, start, start + 5)
                    .and_then(|s| by_serial.get(&s).copied())
                else {
                    continue;
                };
                if to == from {
                    continue;
                }
                // 同一根键在两个原子的 CONECT 里各写一次，去重后才是一根。
                let key = (from.min(to), from.max(to));
                if seen.insert(key) {
                    molecule.bonds.push(key);
                }
            }
        }
    }
    if molecule.atoms.is_empty() {
        return Err(bad("PDB 里没有原子"));
    }
    Ok(molecule)
}

/// 按列取一个整数。PDB 是**定宽**格式，不能按空格切分——
/// 原子序号上万之后几个字段会连在一起，切分出来的列会整体错位。
fn column_u32(line: &str, start: usize, end: usize) -> Option<u32> {
    line.get(start..end.min(line.len()))?.trim().parse().ok()
}

fn columns_vec3(line: &str) -> Option<Vec3> {
    let number = |start: usize, end: usize| -> Option<f32> {
        line.get(start..end.min(line.len()))?.trim().parse().ok()
    };
    let position = Vec3::new(number(30, 38)?, number(38, 46)?, number(46, 54)?);
    position.is_finite().then_some(position)
}

/// 元素符号：先看第 77–78 列，为空再退回第 13–14 列的原子名。
///
/// 两列都可能带电荷后缀（`C+0`）或数字（`C1`），只留字母。
fn element_symbol(line: &str) -> String {
    let letters = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_lowercase()
    };
    let standard = line.get(76..78).map(letters).unwrap_or_default();
    if !standard.is_empty() {
        return standard;
    }
    line.get(12..14).map(letters).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATER: &str = "\
ATOM      1  O           0       0.000   0.000   0.000  0.00  0.00           O
ATOM      2  H           0       0.757   0.586   0.000  0.00  0.00           H
ATOM      3  H           0      -0.757   0.586   0.000  0.00  0.00           H
CONECT    1    2    3
CONECT    2    1
END
";

    #[test]
    fn reads_atoms_and_deduplicates_bonds() {
        let molecule = parse(WATER.as_bytes()).unwrap();
        assert_eq!(molecule.atoms.len(), 3);
        // 1–2 这根键在两条 CONECT 里各写了一次，只该留一根。
        assert_eq!(molecule.bonds, vec![(0, 1), (0, 2)]);
    }

    #[test]
    fn oxygen_is_red_and_hydrogen_is_white() {
        let molecule = parse(WATER.as_bytes()).unwrap();
        assert_eq!(molecule.symbol(&molecule.atoms[0]), "o");
        assert!(molecule.atoms[0].color.x > 0.9 && molecule.atoms[0].color.y < 0.2);
        assert_eq!(molecule.atoms[1].color, Vec3::ONE);
    }

    /// 半数样本文件根本没有第 77 列，元素只能从原子名取。
    #[test]
    fn a_short_line_falls_back_to_the_atom_name_column() {
        let short = "ATOM      1 CL          0       0.000   0.000   0.000\n";
        let molecule = parse(short.as_bytes()).unwrap();
        assert_eq!(molecule.symbol(&molecule.atoms[0]), "cl");
    }

    #[test]
    fn an_unknown_element_is_magenta_rather_than_a_plausible_default() {
        let line = "ATOM      1  Xx          0       0.000   0.000   0.000\n";
        let molecule = parse(line.as_bytes()).unwrap();
        assert_eq!(molecule.atoms[0].color, UNKNOWN);
    }

    #[test]
    fn a_file_without_atoms_is_an_error() {
        assert!(parse(b"HEADER something\nEND\n").is_err());
    }
}
