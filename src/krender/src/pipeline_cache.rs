//! 管线缓存落盘：驱动编译好的管线二进制存到用户缓存目录，下次启动带上。
//!
//! 只在适配器支持 `PIPELINE_CACHE` 时有（目前是 Vulkan 后端）。文件名用
//! [`wgpu::util::pipeline_cache_key`]：同一块显卡、同一个驱动版本才认，换了就重新攒。
//! wgpu 在 `fallback: true` 下会校验文件头，文件坏了或对不上时退回空缓存，不会出错。
//!
//! 设 `KENGINE_PIPELINE_CACHE=0` 可以关掉（量冷启动时用）。

use std::path::PathBuf;

/// 一份带落盘路径的管线缓存。
pub(crate) struct DiskPipelineCache {
    pub(crate) cache: wgpu::PipelineCache,
    path: PathBuf,
    /// 上次写盘时的大小。没长就不写，免得每次退出都重写几兆。
    saved_len: usize,
}

impl DiskPipelineCache {
    /// 读盘上的缓存（没有就建一份空的）。适配器不支持或找不到缓存目录时返回 `None`。
    pub(crate) fn open(device: &wgpu::Device, adapter: &wgpu::Adapter) -> Option<Self> {
        if !device.features().contains(wgpu::Features::PIPELINE_CACHE)
            || std::env::var("KENGINE_PIPELINE_CACHE").is_ok_and(|v| v == "0")
        {
            return None;
        }
        let key = wgpu::util::pipeline_cache_key(&adapter.get_info())?;
        let path = cache_dir()?.join("kengine").join(key);
        let data = std::fs::read(&path).ok();
        // SAFETY: `data` 要么是空，要么是上一次 `get_data` 写下的字节。`fallback: true`
        // 让 wgpu 校验头部（驱动、设备、版本），不匹配时丢掉数据建空缓存，而不是把坏数据交给驱动。
        let cache = unsafe {
            device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("kengine pipeline cache"),
                data: data.as_deref(),
                fallback: true,
            })
        };
        let saved_len = data.map_or(0, |d| d.len());
        klog::debug!("管线缓存：{}（{} 字节）", path.display(), saved_len);
        Some(Self {
            cache,
            path,
            saved_len,
        })
    }

    /// 写盘。先写临时文件再改名，进程中途被杀也不会留下半个文件。
    pub(crate) fn save(&mut self) {
        let Some(data) = self.cache.get_data() else {
            return;
        };
        if data.len() <= self.saved_len {
            return;
        }
        let Some(dir) = self.path.parent() else {
            return;
        };
        let temporary = self.path.with_extension("tmp");
        let result = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&temporary, &data))
            .and_then(|()| std::fs::rename(&temporary, &self.path));
        match result {
            Ok(()) => {
                klog::debug!("管线缓存已写盘：{} 字节", data.len());
                self.saved_len = data.len();
            }
            Err(error) => klog::warn!("管线缓存写盘失败：{error}"),
        }
    }
}

/// 用户缓存目录：Windows 的 `%LOCALAPPDATA%`，macOS 的 `~/Library/Caches`，其余 `$XDG_CACHE_HOME` 或 `~/.cache`。
fn cache_dir() -> Option<PathBuf> {
    let env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        return env("LOCALAPPDATA");
    }
    if cfg!(target_os = "macos") {
        return env("HOME").map(|home| home.join("Library/Caches"));
    }
    env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|home| home.join(".cache")))
}
