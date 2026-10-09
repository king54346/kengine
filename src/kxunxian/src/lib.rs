//! 寻仙资源读取：直接读解包出来的原始文件（`.cct` / `.cmf` / `.pmf` / `.psf` / `.paf` / `.dds`），
//! 拼成引擎的 [`kgltf::Model`] 和 [`kanim::AnimationClip`]，不经过 Blender 导出。
//!
//! 为什么不用导好的 GLB：玩家角色（`zj_*`）是整座衣柜——几千个部件，游戏按装备挑几件穿上；
//! 武器（`wqa`）、法宝（`fba`）是不带骨骼的刚体，运行时挂到角色的挂点骨骼上。这两件事都得在运行时做，
//! 烘成 GLB 就只能「全部塞进去」（一个角色两百兆）或者「只有一套」。
//!
//! ```ignore
//! let mut library = kxunxian::Library::new("assets/unpack2");
//! let character = library.character("zj_shusheng_007")?;
//! let outfit = kxunxian::Outfit {
//!     parts: vec!["m_yf001_st_001".into(), "m_kz001_kz_001".into()],
//!     equips: vec!["e_wqa_101".into(), "e_fba_101".into()],
//! };
//! let model = library.build(&character, &outfit);
//! let idle = library.animation(&character, "zl01")?;
//! ```
//!
//! 格式细节照 XunxianDpkViewer 的解析器（见 `PIPELINE_NOTES.md`）。

pub mod config;
pub mod formats;
pub mod xml;

mod character;

pub use character::{Character, Error, Library, Outfit};
pub use config::{CharacterDef, EquipDef, Hinge, MaterialDef, ModelDef};
