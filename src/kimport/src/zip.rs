//! 只读的 ZIP 容器：KMZ、3MF、压缩的 AMF、USDZ 都是 ZIP。
//!
//! 只实现这几个格式用得到的部分：中央目录（含 ZIP64 扩展）、「存储」与
//! 「deflate」两种压缩方式。不支持加密、分卷——遇到时明确报错而不是读出
//! 垃圾。（ZIP64 是真会遇到的：`truck.3mf` 就是一个很小的 ZIP64 文件，
//! 有些打包工具不管大小一律写 ZIP64 头。）
//!
//! 为什么不用 `zip` crate：它默认拖进 bzip2 / zstd / aes 一串依赖，
//! 而这里用得到的全部逻辑就是下面这一百来行；deflate 本身仍然复用
//! `flate2`（纯 Rust 的 miniz_oxide 后端）。

use crate::bad;
use kasset::LoadError;
use std::io::Read;

/// 一个条目。
#[derive(Debug, Clone)]
pub struct Entry {
    /// 条目路径（ZIP 里总是 `/` 分隔）。
    pub name: String,
    method: u16,
    compressed: usize,
    uncompressed: usize,
    local_offset: usize,
}

/// 打开的 ZIP。
#[derive(Debug)]
pub struct Archive<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, LoadError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| bad("ZIP 被截断"))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, LoadError> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().expect("长度刚好是 8")))
        .ok_or_else(|| bad("ZIP 被截断"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, LoadError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("ZIP 被截断"))
}

/// 文件头是不是 ZIP。
pub fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

impl<'a> Archive<'a> {
    /// 读中央目录。
    pub fn open(bytes: &'a [u8]) -> Result<Self, LoadError> {
        // 目录尾记录在最后 22 + 65535（注释）字节里，从后往前找签名。
        let start = bytes.len().saturating_sub(22 + 65535);
        let eocd = (start..bytes.len().saturating_sub(21))
            .rev()
            .find(|&at| bytes[at..at + 4] == *b"PK\x05\x06")
            .ok_or_else(|| bad("不是 ZIP：找不到中央目录"))?;
        let mut count = u16_at(bytes, eocd + 10)? as usize;
        let mut at = u32_at(bytes, eocd + 16)? as usize;
        if count == 0xffff || at == 0xffff_ffff {
            // ZIP64：目录尾之前 20 字节是定位记录，它指向真正的 ZIP64 目录尾。
            let locator = eocd.checked_sub(20).ok_or_else(|| bad("ZIP64 定位记录缺失"))?;
            if u32_at(bytes, locator)? != 0x0706_4b50 {
                return Err(bad("ZIP64 定位记录缺失"));
            }
            let record = u64_at(bytes, locator + 8)? as usize;
            if u32_at(bytes, record)? != 0x0606_4b50 {
                return Err(bad("ZIP64 目录尾损坏"));
            }
            count = u64_at(bytes, record + 32)? as usize;
            at = u64_at(bytes, record + 48)? as usize;
        }
        let mut entries = Vec::with_capacity(count.min(65536));
        for _ in 0..count {
            if u32_at(bytes, at)? != 0x0201_4b50 {
                return Err(bad("ZIP 中央目录损坏"));
            }
            let flags = u16_at(bytes, at + 8)?;
            if flags & 1 != 0 {
                return Err(bad("不支持加密的 ZIP"));
            }
            let name_len = u16_at(bytes, at + 28)? as usize;
            let extra_len = u16_at(bytes, at + 30)? as usize;
            let comment_len = u16_at(bytes, at + 32)? as usize;
            let name = bytes
                .get(at + 46..at + 46 + name_len)
                .ok_or_else(|| bad("ZIP 被截断"))?;
            let mut compressed = u32_at(bytes, at + 20)? as u64;
            let mut uncompressed = u32_at(bytes, at + 24)? as u64;
            let mut local_offset = u32_at(bytes, at + 42)? as u64;
            // ZIP64 扩展字段（id 1）：值为 0xFFFFFFFF 的那几项按固定顺序放在这里。
            let mut extra = at + 46 + name_len;
            let extra_end = extra + extra_len;
            while extra + 4 <= extra_end {
                let id = u16_at(bytes, extra)?;
                let size = u16_at(bytes, extra + 2)? as usize;
                if id == 1 {
                    let mut field = extra + 4;
                    for value in [&mut uncompressed, &mut compressed, &mut local_offset] {
                        if *value == 0xffff_ffff && field + 8 <= extra + 4 + size {
                            *value = u64_at(bytes, field)?;
                            field += 8;
                        }
                    }
                }
                extra += 4 + size;
            }
            entries.push(Entry {
                name: String::from_utf8_lossy(name).replace('\\', "/"),
                method: u16_at(bytes, at + 10)?,
                compressed: compressed as usize,
                uncompressed: uncompressed as usize,
                local_offset: local_offset as usize,
            });
            at += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { bytes, entries })
    }

    /// 全部条目。
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 按路径找条目（大小写不敏感，忽略开头的 `/`）。
    pub fn find(&self, name: &str) -> Option<&Entry> {
        let name = name.trim_start_matches('/');
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// 第一个扩展名匹配的条目（例如 KMZ 里的 `.dae`）。
    pub fn find_extension(&self, extension: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| {
            e.name
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
        })
    }

    /// 解压一个条目。
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, LoadError> {
        let at = entry.local_offset;
        if u32_at(self.bytes, at)? != 0x0403_4b50 {
            return Err(bad(format!("ZIP 条目 {} 的本地头损坏", entry.name)));
        }
        let name_len = u16_at(self.bytes, at + 26)? as usize;
        let extra_len = u16_at(self.bytes, at + 28)? as usize;
        let data_start = at + 30 + name_len + extra_len;
        let data = self
            .bytes
            .get(data_start..data_start + entry.compressed)
            .ok_or_else(|| bad(format!("ZIP 条目 {} 被截断", entry.name)))?;
        if entry.uncompressed > 1 << 30 {
            return Err(bad(format!("ZIP 条目 {} 解压后超过 1 GiB", entry.name)));
        }
        match entry.method {
            0 => Ok(data.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(entry.uncompressed);
                flate2::read::DeflateDecoder::new(data)
                    .take(entry.uncompressed as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| bad(format!("ZIP 条目 {} 解压失败：{e}", entry.name)))?;
                Ok(out)
            }
            other => Err(bad(format!("ZIP 条目 {} 用了不支持的压缩方式 {other}", entry.name))),
        }
    }

    /// 按路径读条目。
    pub fn read_named(&self, name: &str) -> Option<Vec<u8>> {
        self.find(name).and_then(|e| self.read(e).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工拼一个只含一个「存储」条目的 ZIP。
    fn stored_zip(name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&[0; 4]); // crc（不校验）
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        let cd = out.len();
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&0u32.to_le_bytes()); // 本地头偏移
        out.extend_from_slice(name.as_bytes());
        let cd_len = out.len() - cd;
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        out.extend_from_slice(&(cd_len as u32).to_le_bytes());
        out.extend_from_slice(&(cd as u32).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn reads_a_stored_entry() {
        let bytes = stored_zip("doc.kml", b"hello");
        let archive = Archive::open(&bytes).unwrap();
        assert_eq!(archive.entries().len(), 1);
        assert_eq!(archive.read_named("DOC.KML").unwrap(), b"hello");
        assert!(archive.find_extension("kml").is_some());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(Archive::open(b"not a zip at all").is_err());
    }
}
