//! kcomponents —— 可复用的场景组件。
//!
//! 每个组件都是普通的结构体：`spawn` 往场景里放东西，`update` 每帧推进。不依赖 kapp，
//! 任何用 kscene + krender 的应用都能直接用。
//!
//! | 组件 | 做什么 |
//! |---|---|
//! | [`ocean::Ocean`] | 无限海面：JONSWAP 谱 + FFT 三级联、LOD 网格、尖浪、多层泡沫、浅水色、折射 |
//! | [`ocean::Buoyancy`] | 浮力：船体上放探针，按海面高度推刚体 |
//! | [`ocean::WakeMap`] | 尾迹泡沫：物体划过水面留下的白沫，随时间消散 |
//! | [`sky::Sky`] | 程序化天空：瑞利 / 米氏散射大气、日盘、体积云；同一套模型烘成环境光 |
//! | [`meadow::Meadow`] | 风格化草地：起伏的地形、几万根随风摆的草叶（逆光透射）、野花 |
//! | [`caustics`] | 水下焦散材质 |
//! | [`underwater`] | 水下后处理：染色、吸收、扭曲 |
//! | [`presets`] | 八套环境预设（天空 + 海况 + 光照一起切） |
//! | [`quality`] | 画质档位 |

pub mod caustics;
pub mod fft;
pub mod meadow;
pub mod ocean;
pub mod presets;
pub mod quality;
pub mod sky;
pub mod underwater;
