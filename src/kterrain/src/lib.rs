//! kterrain — 地形
//!
//! 分四层：
//!
//! | 模块 | 干什么 |
//! |---|---|
//! | [`heightmap`] | 高度值、双线性采样、法线、射线求交 |
//! | [`mesh`] | 分块、LOD、**边界对齐**（防裂缝） |
//! | [`brush`] | 抬升 / 下压 / 抹平 / 压平 / 涂材质 |
//! | [`terrain`] | 门面：记住每块的 LOD，只报告变了的 |
//!
//! 全部是纯数据，不碰 GPU——于是最容易出错的那些地方
//! （LOD 边界裂缝、笔刷覆盖范围、抹平的方向依赖）都能直接测。
pub mod brush;
pub mod heightmap;
pub mod mesh;
pub mod terrain;

pub use brush::{Brush, Operation, SplatMap, apply};
pub use heightmap::Heightmap;
pub use mesh::{Chunk, NeighborLods, lod_for, split};
pub use terrain::{ChunkUpdate, Terrain};
