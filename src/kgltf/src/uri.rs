//! glTF 里的 URI 解析。
//!
//! glTF 的缓冲与图片可以内嵌为 `data:` URI，也可以指向同目录下的外部文件。
//! 外部文件一律通过 [`ResourceIo`] 读取，而不是直接碰 `std::fs`。

use base64::{Engine, engine::general_purpose::STANDARD};
use kasset::{LoadError, ResourceIo};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// 读取一个 glTF URI 指向的字节。
///
/// `base_dir` 是 glTF 文件所在目录，用于解析相对路径。
pub(crate) async fn read_uri(
    uri: &str,
    base_dir: &Path,
    io: &Arc<dyn ResourceIo>,
) -> Result<Vec<u8>, LoadError> {
    if let Some(rest) = uri.strip_prefix("data:") {
        return decode_data_uri(rest);
    }

    // 相对路径里的百分号转义要还原，否则带空格的文件名会找不到。
    let decoded = percent_decode(uri);
    let path = base_dir.join(decoded);
    io.load_file(&path).await
}

/// 解码 `data:[<mime>][;base64],<数据>` 的负载部分。
fn decode_data_uri(rest: &str) -> Result<Vec<u8>, LoadError> {
    let Some((meta, payload)) = rest.split_once(',') else {
        return Err(LoadError::message("data URI 缺少逗号分隔符"));
    };

    if meta.ends_with(";base64") || meta.contains(";base64;") {
        STANDARD.decode(payload).map_err(LoadError::custom)
    } else {
        // 非 base64 的 data URI 是百分号转义的原始文本。
        Ok(percent_decode(payload).into_bytes())
    }
}

/// 还原 `%XX` 转义。无法识别的转义原样保留。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// glTF 文件所在目录，用于解析相对 URI。
pub(crate) fn base_dir(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

#[cfg(test)]
mod test {
    use super::*;
    use kasset::MemoryResourceIo;

    #[test]
    fn decodes_base64_data_uri() {
        let bytes = ktask::block_on(async {
            let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
            read_uri(
                "data:application/octet-stream;base64,SGVsbG8=",
                Path::new(""),
                &io,
            )
            .await
            .unwrap()
        });

        assert_eq!(bytes, b"Hello");
    }

    #[test]
    fn decodes_plain_data_uri() {
        let bytes = ktask::block_on(async {
            let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
            read_uri("data:text/plain,a%20b", Path::new(""), &io)
                .await
                .unwrap()
        });

        assert_eq!(bytes, b"a b");
    }

    #[test]
    fn reads_relative_file_through_io() {
        let mut memory = MemoryResourceIo::new();
        memory.add("models/data.bin", vec![1, 2, 3]);

        let bytes = ktask::block_on(async {
            let io: Arc<dyn ResourceIo> = Arc::new(memory);
            read_uri("data.bin", Path::new("models"), &io).await.unwrap()
        });

        assert_eq!(bytes, vec![1, 2, 3]);
    }

    #[test]
    fn percent_escapes_in_filenames_are_decoded() {
        let mut memory = MemoryResourceIo::new();
        memory.add("my model.bin", vec![7]);

        let bytes = ktask::block_on(async {
            let io: Arc<dyn ResourceIo> = Arc::new(memory);
            read_uri("my%20model.bin", Path::new(""), &io).await.unwrap()
        });

        assert_eq!(bytes, vec![7]);
    }

    #[test]
    fn malformed_data_uri_reports_error() {
        let result = ktask::block_on(async {
            let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
            read_uri("data:no-comma-here", Path::new(""), &io).await
        });

        assert!(result.is_err());
    }

    #[test]
    fn trailing_percent_is_left_alone() {
        // 末尾的 % 不足以构成转义，不能 panic。
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("a%zz"), "a%zz");
    }
}
