pub mod io;
pub mod pool;
pub mod reflect;
pub mod safelock;
pub mod sstorage;
pub mod variable;
pub mod visitor;

pub use safelock::*;
pub use uuid;

use std::path::{Path, PathBuf};

/// 将 `T` 类型的切片重新解释为字节切片。
pub fn array_as_u8_slice<T: Sized>(v: &[T]) -> &'_ [u8] {
    // SAFETY：任何有大小的类型都可以重新解释为相同总长度的字节切片；
    // 生命周期与输入切片绑定。
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// 将 `T` 类型的可变切片重新解释为可变字节切片。
pub fn array_as_u8_slice_mut<T: Sized>(v: &mut [T]) -> &'_ mut [u8] {
    // SAFETY：参见 `array_as_u8_slice`。`T: Sized` 保证通过字节视图写入的
    // 任何位模式都保持在原始分配范围内。
    unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, std::mem::size_of_val(v)) }
}

/// 将路径中的 Windows 反斜杠 `\` 替换为正斜杠 `/`，
/// 使序列化后的路径在所有操作系统上保持一致。
pub fn replace_slashes<P: AsRef<Path>>(path: P) -> PathBuf {
    if path.as_ref().components().count() == 1 {
        // 路径只有一个组件，无需替换。
        path.as_ref().to_owned()
    } else {
        let mut result = PathBuf::new();
        for component in path.as_ref().components() {
            result.push(component);
        }
        PathBuf::from(
            result
                .to_string_lossy()
                .to_string()
                .replace(std::path::MAIN_SEPARATOR, "/"),
        )
    }
}
