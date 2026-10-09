//! 资源包：把散落的文件打成一个。
//!
//! 发布时不希望玩家看到一地散文件，运行时也不希望为每个小资源各开一次
//! 系统调用。[`PackWriter`] 把一批文件写成一个包，[`PackResourceIo`] 让
//! 加载器像读文件一样读它——加载器一行代码都不用改，这正是当初把
//! [`ResourceIo`] 抽出来的原因。
//!
//! # 包的结构
//!
//! ```text
//! "KPAK" | 版本 u32 | 目录长度 u32 | 目录 | 数据区
//!                                     ↑
//!   [路径长度 u32 | 路径 UTF-8 | 偏移 u64 | 存储长度 u64 | 原始长度 u64 | 标志 u32] × N
//! ```
//!
//! 标志的第 0 位表示这一项用 deflate 压过（[`PackWriter::compressed`] 打开）。
//! 版本 1 的包（没有后两个字段、从不压缩）照样能读。
//!
//! # 压缩
//!
//! 逐项压、逐项解：读一个资源只解它自己，不用把整个包解开。压完省不到
//! 一成的项（JPG、PNG、OGG 这些本来就压过的）原样存——解压要花时间，
//! 省不了几个字节就不值得。
//!
//! 目录放在**前面**：读包时只需要顺序读一小段就能建好索引，
//! 不必先跳到文件末尾——这一点在将来接网络流式读取时会变得重要。
//!
//! ```
//! use kasset::{PackWriter, PackResourceIo, ResourceIo};
//!
//! let mut writer = PackWriter::new();
//! writer.add("levels/one.scene", b"scene bytes".to_vec());
//! writer.add("textures/wall.png", b"png bytes".to_vec());
//! let bytes = writer.finish();
//!
//! let io = PackResourceIo::from_bytes(&bytes).unwrap();
//! assert!(io.exists(std::path::Path::new("levels/one.scene")));
//! assert_eq!(io.len(), 2);
//! ```

use crate::{error::LoadError, io::ResourceIo};
use fxhash::FxHashMap;
use ktask::BoxedFuture;
use std::{borrow::Cow, io::Read, path::Path, sync::Arc};

/// 包文件的魔数。
pub const PACK_MAGIC: &[u8; 4] = b"KPAK";

/// 包格式版本。
pub const PACK_VERSION: u32 = 2;

/// 目录项标志：这一项用 deflate 压过。
const FLAG_DEFLATE: u32 = 1;

/// 把路径统一成包内的规范形式。
///
/// Windows 上写出来的 `levels\one.scene` 与 Linux 上的 `levels/one.scene`
/// 必须指向同一项，否则同一个包换台机器就读不出来了。
fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 组装一个资源包。
#[derive(Debug, Default)]
pub struct PackWriter {
    entries: Vec<(String, Vec<u8>)>,
    /// 写出时逐项尝试 deflate。
    compress: bool,
}

impl PackWriter {
    /// 新建一个空包。
    pub fn new() -> Self {
        Self::default()
    }

    /// 写出时逐项 deflate 压缩（省不到一成的项原样存）。
    ///
    /// 场景、着色器、glTF 的 JSON、未压缩的音频这些文本 / 原始数据通常能省
    /// 一半以上；已经压过的图片、音频几乎不变，会被自动跳过。
    pub fn compressed(mut self) -> Self {
        self.compress = true;
        self
    }

    /// 放入一份内容。同名项后放的会覆盖先放的。
    pub fn add(&mut self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) {
        let name = normalize(path.as_ref());
        let contents = contents.into();
        match self
            .entries
            .iter_mut()
            .find(|(existing, _)| *existing == name)
        {
            Some(slot) => slot.1 = contents,
            None => self.entries.push((name, contents)),
        }
    }

    /// 从磁盘读一个文件放进包里，包内路径由调用方指定。
    pub fn add_file(
        &mut self,
        pack_path: impl AsRef<Path>,
        disk_path: impl AsRef<Path>,
    ) -> std::io::Result<()> {
        let contents = std::fs::read(disk_path)?;
        self.add(pack_path, contents);
        Ok(())
    }

    /// 递归收进一整个目录，包内路径是相对 `root` 的。
    pub fn add_directory(&mut self, root: impl AsRef<Path>) -> std::io::Result<usize> {
        let root = root.as_ref();
        let mut added = 0;
        let mut stack = vec![root.to_path_buf()];

        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory)? {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                    self.add_file(relative, &path)?;
                    added += 1;
                }
            }
        }

        Ok(added)
    }

    /// 包内的项数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 包里是否什么都没有。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 写成字节。
    pub fn finish(self) -> Vec<u8> {
        // 目录按路径排序：同样的一批文件必然打出同样的包，
        // 增量发布时才能靠比对哈希判断「这个包没变」。
        let mut entries = self.entries;
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // 每项最终存什么：(存储的字节, 原始长度, 标志)。
        let stored: Vec<(Cow<'_, [u8]>, u64, u32)> = entries
            .iter()
            .map(|(_, contents)| {
                let raw = contents.len() as u64;
                if self.compress && contents.len() >= 64 {
                    let packed = deflate(contents);
                    // 省不到一成就不压：解压的时间换不回几个字节。
                    if (packed.len() as f64) < contents.len() as f64 * 0.9 {
                        return (Cow::Owned(packed), raw, FLAG_DEFLATE);
                    }
                }
                (Cow::Borrowed(contents.as_slice()), raw, 0)
            })
            .collect();

        let mut directory = Vec::new();
        let mut offset: u64 = 0;
        for ((name, _), (bytes, raw, flags)) in entries.iter().zip(&stored) {
            let name = name.as_bytes();
            directory.extend_from_slice(&(name.len() as u32).to_le_bytes());
            directory.extend_from_slice(name);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            directory.extend_from_slice(&raw.to_le_bytes());
            directory.extend_from_slice(&flags.to_le_bytes());
            offset += bytes.len() as u64;
        }

        let mut out = Vec::with_capacity(16 + directory.len() + offset as usize);
        out.extend_from_slice(PACK_MAGIC);
        out.extend_from_slice(&PACK_VERSION.to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&directory);
        for (bytes, _, _) in &stored {
            out.extend_from_slice(bytes);
        }
        out
    }

    /// 写成文件。
    pub fn write_to_file(self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, self.finish())
    }
}

/// 从资源包读取的 [`ResourceIo`]。
///
/// 整个包一次性读进内存。包通常是几十到几百 MB 的量级，常驻内存换来的是
/// 零系统调用的资源读取；真要处理超大包，应当换成 mmap 或按需读取，
/// 那时只需要换这一个类型的实现。
pub struct PackResourceIo {
    data: Vec<u8>,
    /// 路径 → 数据区里的这一项。
    index: FxHashMap<String, Entry>,
    /// 数据区在 `data` 里的起始位置。
    body: usize,
}

impl std::fmt::Debug for PackResourceIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackResourceIo")
            .field("entries", &self.index.len())
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// 目录里的一项。
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// 在数据区里的偏移。
    offset: usize,
    /// 存在包里的字节数（压过的话是压缩后的）。
    stored: usize,
    /// 原始字节数。
    raw: usize,
    flags: u32,
}

/// deflate 压一段字节。
fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    // 写进 Vec 不会失败。
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

/// 解析资源包时出的错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    /// 开头不是 `KPAK`，多半根本不是包文件。
    BadMagic,
    /// 包的格式版本不认识。
    UnsupportedVersion(u32),
    /// 文件在读完之前就结束了。
    Truncated,
    /// 某一项的路径不是合法 UTF-8。
    BadPath,
    /// 某一项声明的范围超出了数据区。
    OutOfBounds(String),
    /// 某一项的压缩数据解不开，或者解出来的长度不对。
    Corrupt(String),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "不是资源包（开头不是 KPAK）"),
            Self::UnsupportedVersion(v) => write!(f, "资源包版本 {v} 不受支持"),
            Self::Truncated => write!(f, "资源包被截断了"),
            Self::BadPath => write!(f, "资源包里有非 UTF-8 的路径"),
            Self::OutOfBounds(name) => write!(f, "「{name}」声明的数据范围超出了包"),
            Self::Corrupt(name) => write!(f, "「{name}」的压缩数据损坏"),
        }
    }
}

impl std::error::Error for PackError {}

impl PackResourceIo {
    /// 从字节解析。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PackError> {
        Self::from_vec(bytes.to_vec())
    }

    /// 从已有的字节缓冲解析，不额外拷贝。
    pub fn from_vec(data: Vec<u8>) -> Result<Self, PackError> {
        if data.len() < 12 {
            return Err(PackError::Truncated);
        }
        if &data[0..4] != PACK_MAGIC {
            return Err(PackError::BadMagic);
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        // 版本 1：没有原始长度和标志两个字段，从不压缩。照样能读。
        if version != 1 && version != PACK_VERSION {
            return Err(PackError::UnsupportedVersion(version));
        }
        let extra = if version >= 2 { 12 } else { 0 };
        let directory_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        let body = 12 + directory_len;
        if data.len() < body {
            return Err(PackError::Truncated);
        }

        let mut index = FxHashMap::default();
        let mut cursor = 12;
        while cursor < body {
            if cursor + 4 > body {
                return Err(PackError::Truncated);
            }
            let name_len =
                u32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap()) as usize;
            cursor += 4;

            if cursor + name_len + 16 + extra > body {
                return Err(PackError::Truncated);
            }
            let name = std::str::from_utf8(&data[cursor..cursor + name_len])
                .map_err(|_| PackError::BadPath)?
                .to_string();
            cursor += name_len;

            let offset = u64::from_le_bytes(data[cursor..cursor + 8].try_into().unwrap()) as usize;
            cursor += 8;
            let stored = u64::from_le_bytes(data[cursor..cursor + 8].try_into().unwrap()) as usize;
            cursor += 8;
            let (raw, flags) = if version >= 2 {
                let raw = u64::from_le_bytes(data[cursor..cursor + 8].try_into().unwrap()) as usize;
                let flags = u32::from_le_bytes(data[cursor + 8..cursor + 12].try_into().unwrap());
                cursor += 12;
                (raw, flags)
            } else {
                (stored, 0)
            };

            // 越界的项要在打开包的时候就发现，而不是等到某个资源被请求时才炸。
            if body
                .checked_add(offset)
                .and_then(|v| v.checked_add(stored))
                .is_none_or(|end| end > data.len())
            {
                return Err(PackError::OutOfBounds(name));
            }
            index.insert(
                name,
                Entry {
                    offset,
                    stored,
                    raw,
                    flags,
                },
            );
        }

        Ok(Self { data, index, body })
    }

    /// 从文件读取。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let data = std::fs::read(path).map_err(|_| PackError::Truncated)?;
        Self::from_vec(data)
    }

    /// 包里的项数。
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 包里是否什么都没有。
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// 列出包里所有的路径。
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }

    /// 取某一项的内容。没压缩的直接借包里的那一段，压过的现解一份。
    ///
    /// 解不开（包损坏）时返回 [`None`] 并记一条日志。
    pub fn get(&self, path: &Path) -> Option<Cow<'_, [u8]>> {
        let name = normalize(path);
        let entry = *self.index.get(&name)?;
        let bytes = &self.data[self.body + entry.offset..self.body + entry.offset + entry.stored];
        if entry.flags & FLAG_DEFLATE == 0 {
            return Some(Cow::Borrowed(bytes));
        }
        let mut out = Vec::with_capacity(entry.raw);
        let ok = flate2::read::DeflateDecoder::new(bytes)
            .take(entry.raw as u64 + 1)
            .read_to_end(&mut out)
            .is_ok();
        if !ok || out.len() != entry.raw {
            klog::error!("{}", PackError::Corrupt(name));
            return None;
        }
        Some(Cow::Owned(out))
    }

    /// 某一项在包里是不是压过的。
    pub fn is_compressed(&self, path: &Path) -> bool {
        self.index
            .get(&normalize(path))
            .is_some_and(|e| e.flags & FLAG_DEFLATE != 0)
    }
}

impl ResourceIo for PackResourceIo {
    fn load_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, LoadError>> {
        Box::pin(async move {
            self.get(path)
                .map(Cow::into_owned)
                .ok_or_else(|| LoadError::Io {
                    path: path.to_path_buf(),
                    source: Arc::new(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "资源包里没有这个路径",
                    )),
                })
        })
    }

    fn exists(&self, path: &Path) -> bool {
        self.index.contains_key(&normalize(path))
    }
}

/// 按顺序尝试多个 IO 后端，谁先有就用谁的。
///
/// 典型用法是「散文件优先，资源包兜底」：开发时改一个贴图，往
/// `assets/` 里一放就生效，不必重新打包；发布时目录不存在，自动落到包上。
pub struct LayeredResourceIo {
    layers: Vec<Arc<dyn ResourceIo>>,
}

impl std::fmt::Debug for LayeredResourceIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayeredResourceIo")
            .field("layers", &self.layers.len())
            .finish()
    }
}

impl LayeredResourceIo {
    /// 建一个空的分层 IO。
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// 追加一层，越先加的优先级越高。
    pub fn with(mut self, layer: Arc<dyn ResourceIo>) -> Self {
        self.layers.push(layer);
        self
    }

    /// 层数。
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// 是否一层都没有。
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

impl Default for LayeredResourceIo {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceIo for LayeredResourceIo {
    fn load_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, LoadError>> {
        Box::pin(async move {
            let mut last = None;
            for layer in &self.layers {
                if !layer.exists(path) {
                    continue;
                }
                match layer.load_file(path).await {
                    Ok(bytes) => return Ok(bytes),
                    // 这一层说有、读却失败了（文件正好被删、包坏了），
                    // 继续往下找，但把错误留着——全都失败时报最后一个，
                    // 比报「找不到」更接近真相。
                    Err(error) => last = Some(error),
                }
            }
            Err(last.unwrap_or_else(|| LoadError::Io {
                path: path.to_path_buf(),
                source: Arc::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "所有 IO 层里都没有这个路径",
                )),
            }))
        })
    }

    fn exists(&self, path: &Path) -> bool {
        self.layers.iter().any(|layer| layer.exists(path))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::MemoryResourceIo;

    fn sample() -> Vec<u8> {
        let mut writer = PackWriter::new();
        writer.add("a.txt", b"hello".to_vec());
        writer.add("nested/b.bin", vec![1u8, 2, 3, 4]);
        writer.add("empty.dat", Vec::new());
        writer.finish()
    }

    fn read(io: &dyn ResourceIo, path: &str) -> Result<Vec<u8>, LoadError> {
        ktask::block_on(io.load_file(Path::new(path)))
    }

    #[test]
    fn a_compressed_pack_roundtrips_and_skips_incompressible_entries() {
        let text = "hello kengine ".repeat(500).into_bytes();
        // 伪随机字节：压不动，应当原样存。
        let mut state = 0x1234_5678u32;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let mut writer = PackWriter::new().compressed();
        writer.add("text.json", text.clone());
        writer.add("noise.bin", noise.clone());
        writer.add("tiny.txt", b"hi".to_vec());
        let bytes = writer.finish();
        assert!(
            bytes.len() < text.len() / 4 + noise.len() + 200,
            "文本没被压缩：{} 字节",
            bytes.len()
        );

        let io = PackResourceIo::from_bytes(&bytes).unwrap();
        assert!(io.is_compressed(Path::new("text.json")));
        assert!(
            !io.is_compressed(Path::new("noise.bin")),
            "压不动的项该原样存"
        );
        assert!(!io.is_compressed(Path::new("tiny.txt")), "太小的项不值得压");
        assert_eq!(read(&io, "text.json").unwrap(), text);
        assert_eq!(read(&io, "noise.bin").unwrap(), noise);
        assert_eq!(read(&io, "tiny.txt").unwrap(), b"hi");
    }

    #[test]
    fn a_version_1_pack_still_opens() {
        // 手工拼一个旧格式的包：目录项只有 路径 + 偏移 + 长度。
        let name = b"a.txt";
        let mut directory = Vec::new();
        directory.extend_from_slice(&(name.len() as u32).to_le_bytes());
        directory.extend_from_slice(name);
        directory.extend_from_slice(&0u64.to_le_bytes());
        directory.extend_from_slice(&5u64.to_le_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(PACK_MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(b"hello");

        let io = PackResourceIo::from_bytes(&bytes).unwrap();
        assert_eq!(read(&io, "a.txt").unwrap(), b"hello");
    }

    #[test]
    fn corrupt_compressed_data_fails_the_read_not_the_process() {
        let mut writer = PackWriter::new().compressed();
        writer.add("text.json", "abc".repeat(1000).into_bytes());
        let mut bytes = writer.finish();
        let last = bytes.len() - 10;
        for b in &mut bytes[last..] {
            *b ^= 0xff;
        }
        let io = PackResourceIo::from_bytes(&bytes).unwrap();
        assert!(read(&io, "text.json").is_err());
    }

    #[test]
    fn a_pack_roundtrips_every_entry() {
        let io = PackResourceIo::from_bytes(&sample()).unwrap();

        assert_eq!(io.len(), 3);
        assert_eq!(read(&io, "a.txt").unwrap(), b"hello");
        assert_eq!(read(&io, "nested/b.bin").unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(read(&io, "empty.dat").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn exists_matches_what_can_be_read() {
        let io = PackResourceIo::from_bytes(&sample()).unwrap();

        assert!(io.exists(Path::new("a.txt")));
        assert!(!io.exists(Path::new("nope.txt")));
        assert!(read(&io, "nope.txt").is_err());
    }

    #[test]
    fn windows_and_unix_separators_hit_the_same_entry() {
        // 同一个包换台机器就读不出来，是这类格式最常见的坑。
        let io = PackResourceIo::from_bytes(&sample()).unwrap();

        assert!(io.exists(Path::new("nested/b.bin")));
        assert!(io.exists(Path::new("nested\\b.bin")));
        assert_eq!(read(&io, "nested\\b.bin").unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn the_same_inputs_produce_the_same_pack() {
        // 目录按路径排序，于是同样一批文件必然打出同样的字节，
        // 增量发布时才能靠比对哈希判断「这个包没变」。
        let mut a = PackWriter::new();
        a.add("z.txt", b"1".to_vec());
        a.add("a.txt", b"2".to_vec());

        let mut b = PackWriter::new();
        b.add("a.txt", b"2".to_vec());
        b.add("z.txt", b"1".to_vec());

        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn adding_the_same_path_twice_keeps_the_last_one() {
        let mut writer = PackWriter::new();
        writer.add("a.txt", b"old".to_vec());
        writer.add("a.txt", b"new".to_vec());

        let io = PackResourceIo::from_bytes(&writer.finish()).unwrap();

        assert_eq!(io.len(), 1);
        assert_eq!(read(&io, "a.txt").unwrap(), b"new");
    }

    #[test]
    fn an_empty_pack_is_valid() {
        let io = PackResourceIo::from_bytes(&PackWriter::new().finish()).unwrap();

        assert!(io.is_empty());
        assert!(read(&io, "anything").is_err());
    }

    #[test]
    fn garbage_is_rejected_with_a_specific_reason() {
        assert_eq!(
            PackResourceIo::from_bytes(b"not a pack at all").unwrap_err(),
            PackError::BadMagic
        );
        assert_eq!(
            PackResourceIo::from_bytes(b"KPA").unwrap_err(),
            PackError::Truncated
        );
    }

    #[test]
    fn a_future_pack_version_is_rejected() {
        let mut bytes = sample();
        bytes[4..8].copy_from_slice(&999u32.to_le_bytes());

        assert_eq!(
            PackResourceIo::from_bytes(&bytes).unwrap_err(),
            PackError::UnsupportedVersion(999)
        );
    }

    #[test]
    fn a_truncated_pack_is_caught_when_opened_not_when_read() {
        // 坏包要在打开时就发现，而不是等某个资源被请求时才炸——
        // 那时调用栈离问题已经很远了。
        let bytes = sample();
        let broken = &bytes[..bytes.len() - 3];

        assert!(matches!(
            PackResourceIo::from_bytes(broken),
            Err(PackError::OutOfBounds(_)) | Err(PackError::Truncated)
        ));
    }

    #[test]
    fn a_pack_can_be_written_and_reopened_from_disk() {
        let directory = std::env::temp_dir().join("kengine_pack_test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("assets.kpak");

        let mut writer = PackWriter::new();
        writer.add("hello.txt", b"from disk".to_vec());
        writer.write_to_file(&path).unwrap();

        let io = PackResourceIo::open(&path).unwrap();
        assert_eq!(read(&io, "hello.txt").unwrap(), b"from disk");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_directory_can_be_packed_wholesale() {
        let root = std::env::temp_dir().join("kengine_pack_dir_test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("top.txt"), b"top").unwrap();
        std::fs::write(root.join("sub/inner.txt"), b"inner").unwrap();

        let mut writer = PackWriter::new();
        let added = writer.add_directory(&root).unwrap();
        assert_eq!(added, 2);

        let io = PackResourceIo::from_bytes(&writer.finish()).unwrap();
        assert_eq!(read(&io, "top.txt").unwrap(), b"top");
        // 子目录的层级要保留下来，而不是被拍平。
        assert_eq!(read(&io, "sub/inner.txt").unwrap(), b"inner");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn layered_io_prefers_the_earlier_layer() {
        // 开发时改一个贴图往 assets/ 一放就生效，不必重新打包。
        let mut writer = PackWriter::new();
        writer.add("a.txt", b"from pack".to_vec());
        let pack = PackResourceIo::from_bytes(&writer.finish()).unwrap();
        let loose = MemoryResourceIo::new().with("a.txt", b"from disk".to_vec());

        let io = LayeredResourceIo::new()
            .with(Arc::new(loose))
            .with(Arc::new(pack));

        assert_eq!(read(&io, "a.txt").unwrap(), b"from disk");
    }

    #[test]
    fn layered_io_falls_through_to_the_pack() {
        let mut writer = PackWriter::new();
        writer.add("only_in_pack.txt", b"packed".to_vec());
        let pack = PackResourceIo::from_bytes(&writer.finish()).unwrap();
        let loose = MemoryResourceIo::new().with("other.txt", b"loose".to_vec());

        let io = LayeredResourceIo::new()
            .with(Arc::new(loose))
            .with(Arc::new(pack));

        assert_eq!(read(&io, "only_in_pack.txt").unwrap(), b"packed");
        assert!(io.exists(Path::new("other.txt")));
        assert!(!io.exists(Path::new("nowhere.txt")));
        assert!(read(&io, "nowhere.txt").is_err());
    }

    #[test]
    fn an_empty_layered_io_reports_missing_rather_than_panicking() {
        let io = LayeredResourceIo::new();

        assert!(io.is_empty());
        assert!(!io.exists(Path::new("a.txt")));
        assert!(read(&io, "a.txt").is_err());
    }

    #[test]
    fn resources_load_through_a_pack_end_to_end() {
        use crate::{BoxedLoaderFuture, ResourceData, ResourceLoader, ResourceManager};
        use kcore::uuid::{Uuid, uuid};
        use std::path::PathBuf;

        #[derive(Debug, Default)]
        struct Note(String);
        const NOTE_UUID: Uuid = uuid!("6a1d2c93-77b5-4e18-9f02-3c5e8d41b7a6");
        impl ResourceData for Note {
            fn type_uuid(&self) -> Uuid {
                NOTE_UUID
            }
        }

        struct NoteLoader;
        impl ResourceLoader for NoteLoader {
            fn extensions(&self) -> &[&str] {
                &["note"]
            }
            fn data_type_uuid(&self) -> Uuid {
                NOTE_UUID
            }
            fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
                Box::pin(async move {
                    let bytes = io.load_file(&path).await?;
                    Ok(Box::new(Note(String::from_utf8_lossy(&bytes).into_owned()))
                        as Box<dyn ResourceData>)
                })
            }
        }

        let mut writer = PackWriter::new();
        writer.add("notes/hello.note", b"packed content".to_vec());
        let io = PackResourceIo::from_bytes(&writer.finish()).unwrap();

        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(NoteLoader);

        let note = manager
            .request_blocking::<Note>("notes/hello.note")
            .expect("包里的资源该能加载");

        assert_eq!(note.data_ref().unwrap().0, "packed content");
    }
}
