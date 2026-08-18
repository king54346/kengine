//! 资源状态机与非类型化资源句柄。

use crate::{error::LoadError, resource::ResourceData};
use parking_lot::Mutex;
use std::{
    fmt::{self, Debug},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
    task::Waker,
};

/// 资源的三种状态。
#[derive(Debug)]
pub enum ResourceState {
    /// 尚未加载完成。`wakers` 里是正在 `.await` 这个资源的任务。
    Pending {
        /// 等待唤醒的任务。
        wakers: Vec<Waker>,
    },
    /// 加载成功。
    Ok {
        /// 资源数据。
        data: Box<dyn ResourceData>,
    },
    /// 加载失败。
    Failed {
        /// 失败原因。
        error: LoadError,
    },
}

impl ResourceState {
    /// 是否仍在加载。
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    /// 是否加载成功。
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// 是否加载失败。
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// [`UntypedResource`] 的内部数据。路径不可变，故无需加锁。
struct ResourceHeader {
    path: PathBuf,
    state: Mutex<ResourceState>,
}

/// 不带类型参数的资源句柄。
///
/// 克隆是廉价的（只增加引用计数），所有克隆共享同一份状态——
/// 一处加载完成，所有句柄同时可见。
#[derive(Clone)]
pub struct UntypedResource(Arc<ResourceHeader>);

impl UntypedResource {
    /// 创建一个处于加载中状态的资源。
    pub fn new_pending(path: impl Into<PathBuf>) -> Self {
        Self(Arc::new(ResourceHeader {
            path: path.into(),
            state: Mutex::new(ResourceState::Pending { wakers: Vec::new() }),
        }))
    }

    /// 用现成的数据直接创建一个已就绪的资源（内嵌资源、测试等场景）。
    pub fn new_ok(path: impl Into<PathBuf>, data: Box<dyn ResourceData>) -> Self {
        Self(Arc::new(ResourceHeader {
            path: path.into(),
            state: Mutex::new(ResourceState::Ok { data }),
        }))
    }

    /// 资源路径。
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// 以只读方式访问状态。
    pub fn state(&self) -> parking_lot::MutexGuard<'_, ResourceState> {
        self.0.state.lock()
    }

    /// 是否仍在加载。
    pub fn is_loading(&self) -> bool {
        self.state().is_loading()
    }

    /// 是否加载成功。
    pub fn is_ok(&self) -> bool {
        self.state().is_ok()
    }

    /// 是否加载失败。
    pub fn is_failed(&self) -> bool {
        self.state().is_failed()
    }

    /// 取出失败原因；未失败时返回 [`None`]。
    pub fn error(&self) -> Option<LoadError> {
        match &*self.state() {
            ResourceState::Failed { error } => Some(error.clone()),
            _ => None,
        }
    }

    /// 把资源打回「加载中」，为重新加载做准备。
    ///
    /// 已经在等待的任务保持等待——它们等的是「有结果」，重载之后照样会被唤醒。
    pub(crate) fn reset_to_pending(&self) {
        let mut state = self.0.state.lock();
        if let ResourceState::Pending { .. } = &*state {
            return;
        }
        *state = ResourceState::Pending { wakers: Vec::new() };
    }

    /// 提交加载结果，并唤醒所有正在等待的任务。
    pub(crate) fn commit(&self, result: Result<Box<dyn ResourceData>, LoadError>) {
        let new_state = match result {
            Ok(data) => ResourceState::Ok { data },
            Err(error) => ResourceState::Failed { error },
        };

        // 先换状态拿到旧的 waker 列表，再在锁外唤醒，避免唤醒回调重入本锁。
        let wakers = {
            let mut state = self.0.state.lock();
            let old = std::mem::replace(&mut *state, new_state);
            match old {
                ResourceState::Pending { wakers } => wakers,
                _ => Vec::new(),
            }
        };

        for waker in wakers {
            waker.wake();
        }
    }

    /// 注册一个等待唤醒的任务。资源已就绪时立即返回 `false`。
    pub(crate) fn add_waker(&self, waker: &Waker) -> bool {
        let mut state = self.0.state.lock();
        match &mut *state {
            ResourceState::Pending { wakers } => {
                if !wakers.iter().any(|w| w.will_wake(waker)) {
                    wakers.push(waker.clone());
                }
                true
            }
            _ => false,
        }
    }

    /// 指向同一份资源的两个句柄视为相等。
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// 当前句柄数量，用于调试与资源回收统计。
    pub fn use_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }
}

impl Debug for UntypedResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        let status = match &*state {
            ResourceState::Pending { .. } => "加载中",
            ResourceState::Ok { .. } => "已就绪",
            ResourceState::Failed { .. } => "失败",
        };
        write!(f, "UntypedResource({}, {status})", self.0.path.display())
    }
}

impl PartialEq for UntypedResource {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

impl Eq for UntypedResource {}

impl Hash for UntypedResource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state)
    }
}
