
//! Visitor 是一个基于树形结构的序列化/反序列化器，使用中间表示（IR）来存储数据。
//! 数据序列化时，首先被转换为中间表示，然后写入磁盘。
//! 反序列化同理：先将数据（二进制或文本）读取并转换为 IR，再由用户解析。
//!
//! # 概述
//!
//! Visitor 使用树形结构来创建结构化数据存储。基本单元是**节点**——每个节点是数据字段的容器，
//! 包含名称、父节点句柄、子节点句柄列表和数据字段容器。
//! 数据字段是名称与值的键值对，值可以是任意基本 Rust 类型及部分可平凡复制的数据结构（向量、矩阵等）。
//! 判断某个类型是否可作为字段的标准，是其是否可以表示为一段字节序列。
//!
//! 详见 [`Visitor`] 文档。

#![warn(missing_docs)]

pub mod blackboard;
pub mod glam_impls;
pub mod error;
pub mod field;
mod impls;
pub mod pod;
mod reader;
mod writer;

pub use kcore_derive::Visit;

pub mod prelude {
    //! Types to use `#[derive(Visit)]`
    pub use super::{Visit, VisitResult, Visitor};
    pub use crate::visitor::error::VisitError;
}

use crate::pool::PoolError;
use crate::{
    array_as_u8_slice_mut,
    io::{self},
    pool::{Handle, Pool},
    visitor::{
        reader::{ascii::AsciiReader, binary::BinaryReader, Reader},
        writer::{ascii::AsciiWriter, binary::BinaryWriter, Writer},
    },
};
use bitflags::bitflags;
use blackboard::Blackboard;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use error::VisitError;
use field::{Field, FieldKind};
use fxhash::FxHashMap;
use std::{
    any::Any,
    fmt::{Debug, Formatter},
    fs::File,
    hash::Hash,
    io::{BufWriter, Cursor, Read, Write},
    ops::{Deref, DerefMut},
    path::Path,
    rc::Rc,
    sync::Arc,
};

/// Visitor 版本号枚举。
#[repr(u32)]
pub enum VisitorVersion {
    /// Fyrox 1.0 首个稳定版本。
    FirstStableRelease,

    /// ^^ 在此行上方添加新版本 ^^
    ///
    /// 主要规则：
    /// 1) 新版本名应清晰描述变更内容（如 `VectorFlattening`），并添加说明注释。
    /// 2) **不要**为新版本显式指定数值，编译器会自动分配。`Legacy` 变体是例外。
    /// 3) **不要**删除或移动已有版本条目。
    /// 4) `Last` 变体必须始终处于最后。
    Last,
}

/// Visitor 的当前版本号。
pub const CURRENT_VERSION: u32 = (VisitorVersion::Last as u32).saturating_sub(1);

/// 纯数据代理结构。用于将可平凡复制数据的数组（如 `Vec<u8>`）直接序列化为大块数据，
/// 而不是逐字节存储为独立节点（那样非常低效）。
///
/// `BinaryBlob` 与 [`crate::visitor::pod::PodVecView`] 的存储方式类似，
/// 但类型安全性较低。实践中通常用于 `T = u8`（字符串和路径），
/// 但它接受任意 `Copy` 类型，且缺少 `PodVecView` 中用于类型验证的 `type_id` 机制。
pub struct BinaryBlob<'a, T>
where
    T: Copy,
{
    /// A reference to a vector that represents a binary blob.
    pub vec: &'a mut Vec<T>,
}

impl<T> Visit for BinaryBlob<'_, T>
where
    T: Copy + bytemuck::Pod,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        if visitor.reading {
            if let Some(field) = visitor.find_field(name) {
                match &field.kind {
                    FieldKind::BinaryBlob(data) => {
                        let len = data.len() / size_of::<T>();
                        let mut vec = Vec::<T>::with_capacity(len);

                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                data.as_ptr(),
                                array_as_u8_slice_mut(&mut vec).as_mut_ptr(),
                                data.len(),
                            );

                            vec.set_len(len);
                        }

                        *self.vec = vec;

                        Ok(())
                    }
                    _ => Err(VisitError::FieldTypeDoesNotMatch {
                        expected: stringify!(FieldKind::BinaryBlob),
                        actual: format!("{:?}", field.kind),
                    }),
                }
            } else {
                Err(VisitError::field_does_not_exist(name, visitor))
            }
        } else if visitor.find_field(name).is_some() {
            Err(VisitError::FieldAlreadyExists(name.to_owned()))
        } else {
            let node = visitor.current_node();

            let len_bytes = self.vec.len() * std::mem::size_of::<T>();
            let mut bytes = Vec::<u8>::with_capacity(len_bytes);
            bytes.extend_from_slice(unsafe {
                std::slice::from_raw_parts(self.vec.as_ptr() as *const u8, len_bytes)
            });

            node.fields
                .push(Field::new(name, FieldKind::BinaryBlob(bytes)));

            Ok(())
        }
    }
}

/// [Visit::visit] 或 Visitor 编码操作（如 [Visitor::save_binary_to_file]）的结果类型。
/// 无错误时无返回值。
pub type VisitResult = Result<(), VisitError>;

trait VisitableElementaryField {
    fn write(&self, file: &mut dyn Write) -> VisitResult;
    fn read(&mut self, file: &mut dyn Read) -> VisitResult;
}

macro_rules! impl_visitable_elementary_field {
    ($ty:ty, $write:ident, $read:ident $(, $endian:ident)*) => {
        impl VisitableElementaryField for $ty {
            fn write(&self, file: &mut dyn Write) -> VisitResult {
                file.$write::<$($endian)*>(*self)?;
                Ok(())
            }

            fn read(&mut self, file: &mut dyn Read) -> VisitResult {
                *self = file.$read::<$($endian)*>()?;
                Ok(())
            }
        }
    };
}
impl_visitable_elementary_field!(f64, write_f64, read_f64, LittleEndian);
impl_visitable_elementary_field!(f32, write_f32, read_f32, LittleEndian);
impl_visitable_elementary_field!(u8, write_u8, read_u8);
impl_visitable_elementary_field!(i8, write_i8, read_i8);
impl_visitable_elementary_field!(u16, write_u16, read_u16, LittleEndian);
impl_visitable_elementary_field!(i16, write_i16, read_i16, LittleEndian);
impl_visitable_elementary_field!(u32, write_u32, read_u32, LittleEndian);
impl_visitable_elementary_field!(i32, write_i32, read_i32, LittleEndian);
impl_visitable_elementary_field!(u64, write_u64, read_u64, LittleEndian);
impl_visitable_elementary_field!(i64, write_i64, read_i64, LittleEndian);

/// Visitor 节点——数据字段的集合，存在于节点树中。
/// 每个节点有名称，可有父节点和子节点。
/// 节点用于访问无法用简单内存块表示的复杂数据。
#[derive(Debug)]
pub struct VisitorNode {
    name: String,
    fields: Vec<Field>,
    parent: Handle<VisitorNode>,
    children: Vec<Handle<VisitorNode>>,
}

impl VisitorNode {
    fn new(name: &str, parent: Handle<VisitorNode>) -> Self {
        Self {
            name: name.to_owned(),
            fields: Vec::new(),
            parent,
            children: Vec::new(),
        }
    }
}

impl Default for VisitorNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            fields: Vec::new(),
            parent: Handle::NONE,
            children: Vec::new(),
        }
    }
}

/// `RegionGuard` 是 [Visitor] 的包装，在 drop 时自动离开当前区域。
#[must_use = "the guard must be used"]
pub struct RegionGuard<'a>(&'a mut Visitor);

impl Deref for RegionGuard<'_> {
    type Target = Visitor;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl DerefMut for RegionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}

impl Drop for RegionGuard<'_> {
    fn drop(&mut self) {
        // If we acquired RegionGuard instance, then it is safe to assert that
        // `leave_region` was successful.
        self.0.leave_region().unwrap();
    }
}

bitflags! {
    /// Flags that can be used to influence the behavior of [Visit::visit] methods.
    #[derive(Debug)]
    pub struct VisitorFlags: u32 {
        /// 无特殊行为。
        const NONE = 0;
    }
}

/// Visitor is a tree-based serializer/deserializer with intermediate representation for stored data.
/// When data is serialized, it will be transformed into an intermediate representation and only then
/// will be dumped onto the disk. Deserialization is the same: the data (binary or text) is read
/// and converted into an intermediate representation (IR). End users can use this IR to save or load
/// their structures of pretty much any complexity.
///
/// # Overview
///
/// Visitor uses a tree to create structured data storage. Basic unit is a *node* - it is a container
/// for data fields. Each node has a name, handle to parent, set of handles to children nodes and a
/// 数据字段是名称与值的键值对，值可以是任意基本 Rust 类型及部分可平凡复制的数据结构（向量、矩阵等）。
/// 判断某个类型是否可作为字段的标准，是其是否可以表示为一段字节序列。
///
/// 访问时不直接调用 Visitor 方法读写数据，而是由被访问变量的 [Visit::visit] 方法完成读写。
///
/// 例如，`x.visit("MyValue", &mut visitor)` 会执行以下之一：
///
/// 1. 若 `visitor.is_reading()` 为 false，将 `x` 的值写入 `visitor`，键名为 "MyValue"。
/// 2. 若 `visitor.is_reading()` 为 true，从 `visitor` 中读取名为 "MyValue" 的值并赋给 `x`。
///
/// 具体执行哪种操作，由 [Visitor::is_reading()] 的返回值决定。
pub struct Visitor {
    /// 构成此 Visitor 树的所有节点。
    nodes: Pool<VisitorNode>,
    /// 下一个 `Rc` 或 `Arc` 的唯一 ID 计数器。
    unique_id_counter: u64,
    /// 每个 ID 对应的 `Rc` 或 `Arc` 的类型名，用于在类型不匹配时提供类型信息。
    type_name_map: FxHashMap<u64, &'static str>,
    /// 每个 ID 对应的 `Rc` 值，供读取时查找 `Rc`。
    rc_map: FxHashMap<u64, Rc<dyn Any>>,
    /// 每个 ID 对应的 `Arc` 值，供读取时查找 `Arc`。
    arc_map: FxHashMap<u64, Arc<dyn Any + Send + Sync>>,
    /// 读取模式为 true（加载时），写入模式为 false（保存时）。
    reading: bool,
    /// 当前正在读写的节点句柄。
    current_node: Handle<VisitorNode>,
    /// 树根节点的句柄。
    root: Handle<VisitorNode>,
    /// Visitor 的版本号，见 [`VisitorVersion`]。
    version: u32,
    /// 存储读写时可能需要的辅助对象。
    pub blackboard: Blackboard,
    /// 可激活某些 Visit 实现特殊行为的标志。
    pub flags: VisitorFlags,
}

impl Debug for Visitor {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut output = f.debug_struct("Visitor");

        output.field("flags", &self.flags);

        for (i, node) in self.nodes.iter().enumerate() {
            output.field(&format!("node{i}"), node);
        }

        output.finish()
    }
}

/// 可从 [Visitor] 读取或向 Visitor 写入的类型 trait。
///
/// ## 代码生成
///
/// 过程宏可以为该 trait 生成简单实现，覆盖 99% 的场景。示例：
///
/// ```rust
/// use kcore::visitor::prelude::*;
///
/// #[derive(Visit, Default)]
/// struct MyType {
///     field_a: u32,
///     field_b: String
/// }
/// ```
///
/// 生成的代码大致如下：
///
/// ```rust
/// use kcore::visitor::prelude::*;
///
/// struct MyType {
///     field_a: u32,
///     field_b: String
/// }
///
/// impl Visit for MyType {
///     fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
///         let mut region = visitor.enter_region(name)?;
///
///         self.field_a.visit("FieldA", &mut region)?;
///         self.field_b.visit("FieldB", &mut region)?;
///
///         Ok(())
///     }
/// }
/// ```
///
/// ### 类型属性
///
/// - `#[visit(optional)]` — 将类型所有字段标记为可选，序列化/反序列化错误会被抑制。
/// - `#[visit(pre_visit_method = "function_name")]` — 在生成体前调用的函数名。
/// - `#[visit(post_visit_method = "function_name")]` — 在生成体后调用的函数名。
///
/// ### 字段属性
///
/// - `#[visit(skip)]` — 跳过该字段的序列化/反序列化。
/// - `#[visit(rename = "new_name")]` — 覆盖字段名称。
/// - `#[visit(optional)]` — 将该字段标记为可选，错误会被抑制。
pub trait Visit {
    /// 根据 [Visitor::is_reading()] 的值，对此值进行读取或写入。
    ///
    /// # 写入模式
    ///
    /// 给定名称是值在 Visitor 中的键。同一节点下，区域名和字段名不能重复，
    /// 但区域名和字段名可以相同。若名称冲突则返回错误，否则写入数据。
    ///
    /// # 读取模式
    ///
    /// 给定名称是从 Visitor 中查找值的键。若找不到对应的字段或区域则返回错误，
    /// 否则将被访问的值修改为 Visitor 中存储的数据。
    ///
    /// # 由多个字段构成复杂值
    ///
    /// 若表示此值需要多个字段，可使用 [Visitor::enter_region] 在 Visitor 中
    /// 创建子节点，然后在该子节点上读写各字段，避免名称冲突。
    ///
    /// # 特殊实现
    ///
    /// 有特殊需求的类型可以选择以非常规方式读写。例如，某个值可以尝试以多种方式
    /// 读取数据，以保持与旧版本数据格式的向后兼容性。
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult;
}

impl Default for Visitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Visitor 的数据格式。
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[must_use]
pub enum Format {
    /// 未知/不支持的格式。
    Unknown,
    /// 二进制格式。速度最快、体积最小，但版本控制系统无法合并变更，
    /// 因此不适合协作开发。主要用于生产构建。
    Binary,
    /// 文本格式。速度较慢且体积较大，但版本控制系统可合并变更，
    /// 适合协作开发。
    Ascii,
}

impl Visitor {
    /// 写入编码字节时自动追加到开头的二进制魔数序列。
    /// 由 [Visitor::save_binary_to_file]、[Visitor::save_binary_to_memory]
    /// 和 [Visitor::save_binary_to_vec] 写入。
    ///
    /// [Visitor::load_binary_from_file] 和 [Visitor::load_binary_from_memory]
    /// 若开头不是此魔数则返回错误。
    pub const MAGIC_BINARY_CURRENT: &'static str = "FBAF";

    /// 写入 ASCII 编码时自动追加到开头的文本魔数序列。
    /// 由 [Visitor::save_ascii_to_file]、[Visitor::save_ascii_to_memory]
    /// 和 [Visitor::save_ascii_to_string] 写入。
    ///
    /// [Visitor::load_ascii_from_file] 和 [Visitor::load_ascii_from_memory]
    /// 若开头不是此魔数则返回错误。
    pub const MAGIC_ASCII_CURRENT: &'static str = "FTAX";

    /// 检查给定 reader 是否指向受支持的文件格式。
    #[must_use]
    pub fn is_supported(src: &mut dyn Read) -> bool {
        Self::detect_format(src) != Format::Unknown
    }

    /// 尝试从给定 reader 检测文件格式信息。
    pub fn detect_format(src: &mut dyn Read) -> Format {
        let mut magic: [u8; 4] = Default::default();
        if src.read_exact(&mut magic).is_ok() {
            if magic.eq(Visitor::MAGIC_BINARY_CURRENT.as_bytes()) {
                return Format::Binary;
            } else if magic.eq(Visitor::MAGIC_ASCII_CURRENT.as_bytes()) {
                return Format::Ascii;
            }
        }
        Format::Unknown
    }

    /// 尝试从给定字节切片检测文件格式信息。
    pub fn detect_format_from_slice(data: &[u8]) -> Format {
        let mut src = Cursor::new(data);
        Self::detect_format(&mut src)
    }

    /// 创建一个 Visitor，其中只包含一个名为 `__ROOT__` 的节点，它将作为 Visitor 的当前区域。
    pub fn new() -> Self {
        let mut nodes = Pool::new();
        let root = nodes.spawn(VisitorNode::new("__ROOT__", Handle::NONE));
        Self {
            nodes,
            unique_id_counter: 1,
            type_name_map: FxHashMap::default(),
            rc_map: FxHashMap::default(),
            arc_map: FxHashMap::default(),
            reading: false,
            current_node: root,
            root,
            version: CURRENT_VERSION,
            blackboard: Blackboard::new(),
            flags: VisitorFlags::NONE,
        }
    }

    fn gen_unique_id(&mut self) -> u64 {
        let id = self.unique_id_counter;
        self.unique_id_counter += 1;
        id
    }

    fn rc_id<T>(&mut self, rc: &Rc<T>) -> (u64, bool)
    where
        T: Any,
    {
        if let Some(id) = self.rc_map.iter().find_map(|(id, ptr)| {
            if Rc::as_ptr(ptr) as *const T == Rc::as_ptr(rc) {
                Some(*id)
            } else {
                None
            }
        }) {
            (id, false)
        } else {
            let id = self.gen_unique_id();
            self.type_name_map.insert(id, std::any::type_name::<T>());
            self.rc_map.insert(id, rc.clone());
            (id, true)
        }
    }

    fn arc_id<T>(&mut self, arc: &Arc<T>) -> (u64, bool)
    where
        T: Any + Send + Sync,
    {
        if let Some(id) = self.arc_map.iter().find_map(|(id, ptr)| {
            if Arc::as_ptr(ptr) as *const T == Arc::as_ptr(arc) {
                Some(*id)
            } else {
                None
            }
        }) {
            (id, false)
        } else {
            let id = self.gen_unique_id();
            self.type_name_map.insert(id, std::any::type_name::<T>());
            self.arc_map.insert(id, arc.clone());
            (id, true)
        }
    }

    /// 按名称查找字段。
    pub fn find_field(&mut self, name: &str) -> Option<&mut Field> {
        self.nodes
            .borrow_mut(self.current_node)
            .fields
            .iter_mut()
            .find(|field| field.name == name)
    }

    /// 按名称查找节点。
    pub fn find_node(&self, name: &str) -> Option<&VisitorNode> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// 若为 true，Visitor 正在修改被访问的值（加载模式）；
    /// 若为 false，Visitor 正在复制并存储被访问的值（保存模式）。
    pub fn is_reading(&self) -> bool {
        self.reading
    }

    fn current_node(&mut self) -> &mut VisitorNode {
        self.nodes.borrow_mut(self.current_node)
    }

    /// 返回 Visitor 的版本号。
    pub fn version(&self) -> u32 {
        self.version
    }

    /// 返回当前节点是否存在名为 `name` 的子区域。
    /// 读取模式下，返回 true 表示 `enter_region` 会成功；
    /// 写入模式下，返回 true 表示 `enter_region` 会失败。
    pub fn has_region(&self, name: &str) -> bool {
        let node = self.nodes.borrow(self.current_node);
        node.children
            .iter()
            .any(|child| self.nodes.borrow(*child).name == name)
    }

    /// 读取模式：在当前节点的子节点中查找给定名称的节点，返回其 Visitor；不存在则返回错误。
    ///
    /// 写入模式：在当前节点下创建给定名称的子节点，返回其 Visitor；已存在同名节点则返回错误。
    pub fn enter_region(&mut self, name: &str) -> Result<RegionGuard<'_>, VisitError> {
        let node = self.nodes.borrow(self.current_node);
        if self.reading {
            let mut region = Handle::NONE;
            for child_handle in node.children.iter() {
                let child = self.nodes.borrow(*child_handle);
                if child.name == name {
                    region = *child_handle;
                    break;
                }
            }
            if region.is_some() {
                self.current_node = region;
                Ok(RegionGuard(self))
            } else {
                Err(VisitError::RegionDoesNotExist(
                    self.breadcrumbs() + " > " + name,
                ))
            }
        } else {
            // 确保节点尚不存在。
            for child_handle in node.children.iter() {
                let child = self.nodes.borrow(*child_handle);
                if child.name == name {
                    return Err(VisitError::RegionAlreadyExists(name.to_owned()));
                }
            }

            let node_handle = self.nodes.spawn(VisitorNode::new(name, self.current_node));
            self.nodes
                .borrow_mut(self.current_node)
                .children
                .push(node_handle);
            self.current_node = node_handle;

            Ok(RegionGuard(self))
        }
    }

    /// 返回从根节点到当前节点路径的面包屑字符串。
    pub fn breadcrumbs(&self) -> String {
        self.build_breadcrumb(" > ")
    }

    /// 使用指定分隔符构建从根节点到当前节点的路径字符串。
    fn build_breadcrumb(&self, separator: &str) -> String {
        let mut rev = String::new();
        let mut handle = self.current_node;
        loop {
            let node = self.nodes.try_borrow(handle);
            let Ok(node) = node else {
                break;
            };
            if !rev.is_empty() {
                rev.extend(separator.chars().rev());
            }
            rev.extend(node.name.chars().rev());
            handle = node.parent;
        }
        rev.chars().rev().collect()
    }

    /// 返回当前区域名称。正常运行时不会为 None（无法离开初始 `__ROOT__` 区域）。
    pub fn current_region(&self) -> Result<&str, PoolError> {
        self.nodes
            .try_borrow(self.current_node)
            .map(|n| n.name.as_str())
    }

    fn leave_region(&mut self) -> VisitResult {
        self.current_node = self.nodes.borrow(self.current_node).parent;
        if self.current_node.is_none() {
            Err(VisitError::NoActiveNode)
        } else {
            Ok(())
        }
    }

    /// 以人类可读形式获取当前节点内容。
    pub fn debug(&self) -> String {
        let mut w = Cursor::new(Vec::<u8>::new());
        let result = self.debug_to(&mut w);
        match result {
            Ok(()) => String::from_utf8_lossy(w.get_ref()).into_owned(),
            Err(err) => err.to_string(),
        }
    }

    /// 将当前节点内容以人类可读形式写入 writer。
    pub fn debug_to<W: Write>(&self, w: &mut W) -> VisitResult {
        let writer = AsciiWriter::default();
        writer.write_node(self, &self.nodes[self.current_node], 0, w)?;
        writeln!(w)?;
        w.flush()?;
        Ok(())
    }

    /// 将此 Visitor 的所有数据序列化为 ASCII 字符串（每个节点占一行，子节点用制表符缩进）。
    pub fn save_ascii_to_string(&self) -> String {
        let mut cursor = Cursor::<Vec<u8>>::default();
        self.save_ascii_to_memory(&mut cursor).unwrap();
        String::from_utf8(cursor.into_inner()).unwrap()
    }

    /// 将此 Visitor 的所有数据序列化为 ASCII 字符串并保存到指定路径。
    pub fn save_ascii_to_file(&self, path: impl AsRef<Path>) -> VisitResult {
        let mut writer = BufWriter::new(File::create(path)?);
        let text = self.save_ascii_to_string();
        writer.write_all(text.as_bytes())?;
        Ok(())
    }

    /// 将此 Visitor 的所有数据序列化为 ASCII 字符串并写入给定 writer。
    pub fn save_ascii_to_memory(&self, mut dest: impl Write) -> VisitResult {
        let writer = AsciiWriter::default();
        writer.write(self, &mut dest)
    }

    /// Tries to create a visitor from the given data. The returned instance can then be used to
    /// deserialize some data.
    pub fn load_ascii_from_memory(data: &[u8]) -> Result<Self, VisitError> {
        let mut src = Cursor::new(data);
        let mut reader = AsciiReader::new(&mut src);
        reader.read()
    }

    /// 从给定文件创建 Visitor（ASCII 格式），返回的实例可用于反序列化数据。
    pub async fn load_ascii_from_file(path: impl AsRef<Path>) -> Result<Self, VisitError> {
        Self::load_ascii_from_memory(&io::load_file(path).await?)
    }

    /// 将此 Visitor 数据写入给定 writer（以 [Visitor::MAGIC_BINARY_CURRENT] 开头）。
    pub fn save_binary_to_memory(&self, mut dest: impl Write) -> VisitResult {
        let writer = BinaryWriter::default();
        writer.write(self, &mut dest)
    }

    /// 将此 Visitor 数据编码为字节并追加到 `Vec<u8>`（以 [Visitor::MAGIC_BINARY_CURRENT] 开头）。
    pub fn save_binary_to_vec(&self) -> Result<Vec<u8>, VisitError> {
        let mut writer = Cursor::new(Vec::new());
        self.save_binary_to_memory(&mut writer)?;
        Ok(writer.into_inner())
    }

    /// 在指定路径创建文件并以二进制格式写入此 Visitor 数据，可通过 [Visitor::load_binary_from_file] 还原。
    pub fn save_binary_to_file(&self, path: impl AsRef<Path>) -> VisitResult {
        let writer = BufWriter::new(File::create(path)?);
        self.save_binary_to_memory(writer)
    }

    /// 从给定路径读取并创建 Visitor（二进制格式），假定文件由 [Visitor::save_binary_to_file] 创建。
    pub async fn load_binary_from_file(path: impl AsRef<Path>) -> Result<Self, VisitError> {
        Self::load_binary_from_memory(&io::load_file(path).await?)
    }

    /// 从给定字节切片创建 Visitor（二进制格式），假定字节由 [Visitor::save_binary_to_vec] 产生。
    pub fn load_binary_from_memory(data: &[u8]) -> Result<Self, VisitError> {
        let mut src = Cursor::new(data);
        let mut reader = BinaryReader::new(&mut src);
        reader.read()
    }

    /// 从给定文件加载 Visitor，自动检测格式（二进制或 ASCII）。
    pub async fn load_from_file(path: impl AsRef<Path>) -> Result<Self, VisitError> {
        Self::load_from_memory(&io::load_file(path).await?)
    }

    /// 从给定数据加载 Visitor，自动检测格式（二进制或 ASCII）。
    pub fn load_from_memory(data: &[u8]) -> Result<Self, VisitError> {
        match Self::detect_format_from_slice(data) {
            Format::Unknown => Err(VisitError::NotSupportedFormat),
            Format::Binary => Self::load_binary_from_memory(data),
            Format::Ascii => Self::load_ascii_from_memory(data),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::visitor::{prelude::*, BinaryBlob};
    use nalgebra::{
        Matrix2, Matrix3, Matrix4, UnitComplex, UnitQuaternion, Vector2, Vector3, Vector4,
    };
    use std::sync::Arc;
    use std::{fs::File, io::Write, path::Path, rc, rc::Rc, sync};
    use uuid::{uuid, Uuid};

    #[derive(Visit, Default, PartialEq, Debug)]
    pub struct Model {
        data: u64,
    }

    #[derive(Default, PartialEq, Debug)]
    pub struct Texture {
        data: Vec<u8>,
    }

    impl Visit for Texture {
        fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
            let mut region = visitor.enter_region(name)?;
            let mut proxy = BinaryBlob {
                vec: &mut self.data,
            };
            proxy.visit("Data", &mut region)?;
            Ok(())
        }
    }

    #[allow(dead_code)]
    #[derive(Visit, PartialEq, Debug, Default)]
    pub enum ResourceKind {
        #[default]
        Unknown,
        Model(Model),
        Texture(Texture),
    }

    #[derive(Visit, PartialEq, Debug)]
    struct Resource {
        kind: ResourceKind,
        data: u16,
    }

    impl Resource {
        fn new(kind: ResourceKind) -> Self {
            Self { kind, data: 0 }
        }
    }

    impl Default for Resource {
        fn default() -> Self {
            Self {
                kind: ResourceKind::Unknown,
                data: 0,
            }
        }
    }

    #[derive(Default, Visit, Debug)]
    struct Weaks {
        weak_resource_arc: Option<sync::Weak<Resource>>,
        weak_resource_rc: Option<rc::Weak<Resource>>,
    }

    impl PartialEq for Weaks {
        fn eq(&self, other: &Self) -> bool {
            self.weak_resource_arc.as_ref().and_then(|r| r.upgrade())
                == other.weak_resource_arc.as_ref().and_then(|r| r.upgrade())
                && self.weak_resource_rc.as_ref().and_then(|r| r.upgrade())
                    == other.weak_resource_rc.as_ref().and_then(|r| r.upgrade())
        }
    }

    #[derive(Default, Visit, Debug, PartialEq)]
    struct Foo {
        boolean: bool,
        num_u8: u8,
        num_i8: i8,
        num_u16: u16,
        num_i16: i16,
        num_u32: u32,
        num_i32: i32,
        num_u64: u64,
        num_i64: i64,
        num_f32: f32,
        num_f64: f64,
        quat: UnitQuaternion<f32>,
        mat4: Matrix4<f32>,
        array: Vec<u8>,
        mat3: Matrix3<f32>,
        uuid: Uuid,
        complex: UnitComplex<f32>,
        mat2: Matrix2<f32>,

        vec2_u8: Vector2<u8>,
        vec2_i8: Vector2<i8>,
        vec2_u16: Vector2<u16>,
        vec2_i16: Vector2<i16>,
        vec2_u32: Vector2<u32>,
        vec2_i32: Vector2<i32>,
        vec2_u64: Vector2<u64>,
        vec2_i64: Vector2<i64>,

        vec3_u8: Vector3<u8>,
        vec3_i8: Vector3<i8>,
        vec3_u16: Vector3<u16>,
        vec3_i16: Vector3<i16>,
        vec3_u32: Vector3<u32>,
        vec3_i32: Vector3<i32>,
        vec3_u64: Vector3<u64>,
        vec3_i64: Vector3<i64>,

        vec4_u8: Vector4<u8>,
        vec4_i8: Vector4<i8>,
        vec4_u16: Vector4<u16>,
        vec4_i16: Vector4<i16>,
        vec4_u32: Vector4<u32>,
        vec4_i32: Vector4<i32>,
        vec4_u64: Vector4<u64>,
        vec4_i64: Vector4<i64>,

        string: String,

        vec2_f32: Vector2<f32>,
        vec2_f64: Vector2<f64>,
        vec3_f32: Vector3<f32>,
        vec3_f64: Vector3<f64>,
        vec4_f32: Vector4<f32>,
        vec4_f64: Vector4<f64>,

        shared_resource: Option<Rc<Resource>>,
        shared_resource_arc: Option<Arc<Resource>>,
        weaks: Weaks,
    }

    impl Foo {
        fn new(resource: Rc<Resource>, arc_resource: Arc<Resource>) -> Self {
            Self {
                boolean: true,
                num_u8: 123,
                num_i8: -123,
                num_u16: 123,
                num_i16: -123,
                num_u32: 123,
                num_i32: -123,
                num_u64: 123,
                num_i64: -123,
                num_f32: 123.321,
                num_f64: 123.321,
                quat: UnitQuaternion::from_euler_angles(1.0, 2.0, 3.0),
                mat4: Matrix4::new_scaling(3.0),
                array: vec![1, 2, 3, 4],
                mat3: Matrix3::new_scaling(3.0),
                uuid: uuid!("51a582c0-30d7-4dbc-b5a0-da8ea186edce"),
                complex: UnitComplex::new(0.0),
                mat2: Matrix2::new_scaling(2.0),
                vec2_u8: Vector2::new(1, 2),
                vec2_i8: Vector2::new(-1, -2),
                vec2_u16: Vector2::new(1, 2),
                vec2_i16: Vector2::new(-1, -2),
                vec2_u32: Vector2::new(1, 2),
                vec2_i32: Vector2::new(-1, -2),
                vec2_u64: Vector2::new(1, 2),
                vec2_i64: Vector2::new(-1, -2),
                vec3_u8: Vector3::new(1, 2, 3),
                vec3_i8: Vector3::new(-1, -2, -3),
                vec3_u16: Vector3::new(1, 2, 3),
                vec3_i16: Vector3::new(-1, -2, -3),
                vec3_u32: Vector3::new(1, 2, 3),
                vec3_i32: Vector3::new(-1, -2, -3),
                vec3_u64: Vector3::new(1, 2, 3),
                vec3_i64: Vector3::new(-1, -2, -3),
                vec4_u8: Vector4::new(1, 2, 3, 4),
                vec4_i8: Vector4::new(-1, -2, -3, -4),
                vec4_u16: Vector4::new(1, 2, 3, 4),
                vec4_i16: Vector4::new(-1, -2, -3, -4),
                vec4_u32: Vector4::new(1, 2, 3, 4),
                vec4_i32: Vector4::new(-1, -2, -3, -4),
                vec4_u64: Vector4::new(1, 2, 3, 4),
                vec4_i64: Vector4::new(-1, -2, -3, -4),
                vec2_f32: Vector2::new(123.321, 234.432),
                vec2_f64: Vector2::new(123.321, 234.432),
                vec3_f32: Vector3::new(123.321, 234.432, 567.765),
                vec3_f64: Vector3::new(123.321, 234.432, 567.765),
                vec4_f32: Vector4::new(123.321, 234.432, 567.765, 890.098),
                vec4_f64: Vector4::new(123.321, 234.432, 567.765, 890.098),
                weaks: Weaks {
                    weak_resource_arc: Some(Arc::downgrade(&arc_resource)),
                    weak_resource_rc: Some(Rc::downgrade(&resource)),
                },
                shared_resource: Some(resource),
                shared_resource_arc: Some(arc_resource),
                string: "This Is A String With Reserved Characters <>:;{}[\\\\\\\\\\] \
                and \"quotes\" many \"\"\"quotes\"\"\"\" and line\nbreak\ttabs\t\t\t\t"
                    .to_string(),
            }
        }
    }

    fn resource() -> Rc<Resource> {
        Rc::new(Resource::new(ResourceKind::Model(Model { data: 555 })))
    }

    fn resource_arc() -> Arc<Resource> {
        Arc::new(Resource::new(ResourceKind::Model(Model { data: 555 })))
    }

    fn objects(resource: Rc<Resource>, arc_resource: Arc<Resource>) -> Vec<Foo> {
        vec![
            Foo::new(resource.clone(), arc_resource.clone()),
            Foo::new(resource, arc_resource),
        ]
    }

    fn serialize() -> Visitor {
        let mut resource = resource();
        let mut resource_arc = resource_arc();
        let mut objects = objects(resource.clone(), resource_arc.clone());

        let mut visitor = Visitor::new();
        resource.visit("SharedResource", &mut visitor).unwrap();
        resource_arc
            .visit("SharedResourceArc", &mut visitor)
            .unwrap();
        objects.visit("Objects", &mut visitor).unwrap();
        visitor
    }

    #[test]
    fn visitor_test_binary() {
        let path = Path::new("test.bin");

        // Save
        {
            let visitor = serialize();

            visitor.save_binary_to_file(path).unwrap();
            if let Ok(mut file) = File::create(Path::new("test.txt")) {
                file.write_all(visitor.save_ascii_to_string().as_bytes())
                    .unwrap();
            }
        }

        // Load
        {
            let expected_resource = resource();
            let expected_resource_arc = resource_arc();
            let expected_objects =
                objects(expected_resource.clone(), expected_resource_arc.clone());

            let mut visitor = futures::executor::block_on(Visitor::load_from_file(path)).unwrap();
            let mut resource: Rc<Resource> = Rc::new(Default::default());
            resource.visit("SharedResource", &mut visitor).unwrap();
            assert_eq!(resource, expected_resource);

            let mut resource_arc: Arc<Resource> = Arc::new(Default::default());
            resource_arc
                .visit("SharedResourceArc", &mut visitor)
                .unwrap();
            assert_eq!(resource_arc, expected_resource_arc);

            let mut objects: Vec<Foo> = Vec::new();
            objects.visit("Objects", &mut visitor).unwrap();
            assert_eq!(objects, expected_objects);
        }
    }

    #[test]
    fn visitor_test_ascii() {
        let path = Path::new("test_ascii.txt");

        // Save
        {
            let visitor = serialize();
            visitor.save_ascii_to_file(path).unwrap();
        }

        // Load
        {
            let expected_resource = resource();
            let expected_resource_arc = resource_arc();
            let expected_objects =
                objects(expected_resource.clone(), expected_resource_arc.clone());

            let mut visitor =
                futures::executor::block_on(Visitor::load_ascii_from_file(path)).unwrap();
            let mut resource: Rc<Resource> = Rc::new(Default::default());
            resource.visit("SharedResource", &mut visitor).unwrap();
            assert_eq!(resource, expected_resource);

            let mut resource_arc: Arc<Resource> = Arc::new(Default::default());
            resource_arc
                .visit("SharedResourceArc", &mut visitor)
                .unwrap();
            assert_eq!(resource_arc, expected_resource_arc);

            let mut objects: Vec<Foo> = Vec::new();
            objects.visit("Objects", &mut visitor).unwrap();
            assert_eq!(objects, expected_objects);
        }
    }
}
