//! kimport —— 第三方资源格式的**原生**导入。
//!
//! 这个 crate 回答的是一个很具体的问题：引擎已经能读 glTF 了，
//! 那些「别人家的格式」（OBJ、STL、PLY、PCD、MD2、VOX、IFC……）怎么办。
//!
//! # 三条规矩
//!
//! 1. **纯 Rust，运行期解析**。不启浏览器、不跑 JavaScript、不预烘中间格式。
//!    读的就是资源本来的那个文件，改了资源不用重新烘一遍。
//! 2. **能复用的绝不重写**。图片解码走 [`ktexture`]，压缩块解码走
//!    `texture2ddecoder`，glTF 走 [`kgltf`]；这里只写引擎里确实没有的部分。
//! 3. **老实说明子集**。IFC、USD、Lottie 这类格式的完整实现是独立项目的规模，
//!    这里实现的是「能把这批例子画出来」的子集，每个模块的文档注释里
//!    写清楚**支持到哪、不支持什么**，而不是假装全都支持。
//!
//! # 产物统一是 [`Model`]
//!
//! 凡是「一堆三角形 + 材质 + 节点树」的格式（OBJ / STL / PLY / MD2 / VOX /
//! LDraw / KMZ / USDZ / IFC）一律产出 [`kgltf::Model`]，于是
//! [`Scene::instantiate_model`](kscene) 那条已有的实例化路径原样可用，
//! 也不必为每种格式再发明一套场景描述。
//!
//! 装不进 `Model` 的才有自己的类型：[`pcd::PointCloud`]（点云）、
//! [`pdb::Molecule`]（原子与化学键）、[`nrrd::Volume`]（体数据）、
//! [`mdd::PointCache`]（逐帧顶点缓存）、[`svg::Document`]（二维路径）、
//! [`lottie::Animation`]（矢量动画）。
//!
//! ```no_run
//! use kasset::ResourceManager;
//! use kgltf::Model;
//!
//! let manager = ResourceManager::new();
//! manager.add_loader(kimport::ObjLoader);
//! let model = manager.request::<Model>("models/obj/male02/male02.obj");
//! ```

#![warn(missing_docs)]

pub mod md2;
pub mod mdd;
pub mod nrrd;
pub mod obj;
pub mod pcd;
pub mod pdb;
pub mod ply;
pub mod stl;
pub mod vox;

use kasset::{LoadError, ResourceIo};
use kgltf::{MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::Material;
use kmesh::Mesh;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub use md2::Md2Loader;
pub use obj::ObjLoader;
pub use ply::PlyLoader;
pub use stl::StlLoader;
pub use vox::VoxLoader;

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        Md2Loader, ObjLoader, PlyLoader, StlLoader, VoxLoader, md2::Md2, mdd::PointCache,
        nrrd::Volume, pcd::PointCloud, pdb::Molecule,
    };
}

/// 这批导入器共用的上限。
///
/// 不是性能调优，是**安全边界**：解析器面对的是外部文件，一个写坏的
/// 头部字段可以让「按 count 预分配」变成几个 GB 的分配。所有从文件里
/// 读出来、又要拿去开数组的数，都先过一遍这里。
pub mod limits {
    /// 单个网格的顶点数上限。
    pub const VERTICES: usize = 32_000_000;
    /// 单个模型的节点数上限。
    pub const NODES: usize = 1_000_000;
    /// 文本格式允许的最大行数，防止病态文件把解析卡死。
    pub const LINES: usize = 40_000_000;
}

/// 构造一条「格式不对」的错误。各模块用得太频繁，抽出来省一行。
pub(crate) fn bad(message: impl Into<String>) -> LoadError {
    LoadError::message(message.into())
}

/// 把单个网格 + 单个材质包成最简单的 [`Model`]：一个根节点，一份几何。
pub fn single_mesh_model(name: &str, mesh: Mesh, material: Material) -> Model {
    Model::new(
        vec![mesh],
        vec![material],
        vec![ModelNode {
            name: name.to_string(),
            transform: NodeTransform::default(),
            children: Vec::new(),
            parts: vec![MeshPart {
                mesh: 0,
                material: Some(0),
            }],
            skin: None,
        }],
        vec![0],
    )
}

/// 把若干「网格 + 材质槽位」摊成一个单层节点树。
///
/// OBJ 的 group、PLY 的多元素、IFC 的构件都是这个形状：没有真正的层级，
/// 只是一批并列的物体。
pub fn flat_model(name: &str, parts: Vec<(String, Mesh, Option<usize>)>, materials: Vec<Material>) -> Model {
    let mut meshes = Vec::with_capacity(parts.len());
    let mut nodes = Vec::with_capacity(parts.len() + 1);
    nodes.push(ModelNode {
        name: name.to_string(),
        transform: NodeTransform::default(),
        children: (1..=parts.len()).collect(),
        parts: Vec::new(),
        skin: None,
    });
    for (index, (child, mesh, material)) in parts.into_iter().enumerate() {
        meshes.push(mesh);
        nodes.push(ModelNode {
            name: child,
            transform: NodeTransform::default(),
            children: Vec::new(),
            parts: vec![MeshPart {
                mesh: index,
                material,
            }],
            skin: None,
        });
    }
    Model::new(meshes, materials, nodes, vec![0])
}

/// 资源路径的所在目录，用于解析格式内部的相对引用（MTL、贴图、零件库）。
pub(crate) fn base_dir(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// 生成一个资源加载器类型。
///
/// 每种格式都要写一遍「实现 `ResourceLoader`、读文件、调解析函数、
/// 装箱」，差别只有扩展名和那一行解析调用。抄十几遍的话，往加载路径上
/// 加一件事（比如统一的耗时日志）就要改十几处。
macro_rules! loader {
    (
        $(#[$meta:meta])*
        $name:ident -> $ty:ty : [$($ext:literal),+ $(,)?] = $uuid:expr, $parse:path
    ) => {
        $(#[$meta])*
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $name;

        impl kasset::ResourceLoader for $name {
            fn extensions(&self) -> &[&str] {
                &[$($ext),+]
            }

            fn data_type_uuid(&self) -> kcore::uuid::Uuid {
                $uuid
            }

            fn load(
                &self,
                path: std::path::PathBuf,
                io: std::sync::Arc<dyn kasset::ResourceIo>,
            ) -> kasset::BoxedLoaderFuture {
                Box::pin(async move {
                    let bytes = io.load_file(&path).await?;
                    let started = std::time::Instant::now();
                    let data = $parse(bytes, path.clone(), io).await?;
                    klog::debug!(
                        "{} 已导入：{}（{:.1} ms）",
                        stringify!($name),
                        path.display(),
                        started.elapsed().as_secs_f32() * 1000.0
                    );
                    Ok(Box::new(data) as Box<dyn kasset::ResourceData>)
                })
            }
        }
    };
}
pub(crate) use loader;

/// 读一个和主资源同目录的附属文件（MTL、贴图、零件），读不到返回 `None`。
///
/// 附属文件缺失是**常态**而不是错误：OBJ 可以没有 MTL，MTL 引用的贴图
/// 可能根本没随模型一起发布。整个导入因为少一张贴图而失败是不可接受的。
pub(crate) async fn sibling(io: &Arc<dyn ResourceIo>, base: &Path, name: &str) -> Option<Vec<u8>> {
    // 格式内部的相对路径经常带 Windows 分隔符，或者 `./` 前缀。
    let cleaned = name.replace('\\', "/");
    let cleaned = cleaned.trim_start_matches("./");
    io.load_file(&base.join(cleaned)).await.ok()
}
