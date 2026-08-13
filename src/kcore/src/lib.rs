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

/// Reinterprets a slice of `T` as a slice of bytes.
pub fn array_as_u8_slice<T: Sized>(v: &[T]) -> &'_ [u8] {
    // SAFETY: Any sized type can be reinterpreted as a byte slice of the same
    // total length; the lifetime is tied to the input slice.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// Reinterprets a mutable slice of `T` as a mutable slice of bytes.
pub fn array_as_u8_slice_mut<T: Sized>(v: &mut [T]) -> &'_ mut [u8] {
    // SAFETY: See `array_as_u8_slice`. `T: Sized` means every bit pattern written
    // through the byte view stays inside the original allocation.
    unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, std::mem::size_of_val(v)) }
}

/// Replaces Windows back slashes `\` with forward slashes `/`, so serialized paths
/// stay portable across all OSes.
pub fn replace_slashes<P: AsRef<Path>>(path: P) -> PathBuf {
    if path.as_ref().components().count() == 1 {
        // The path is a single component, so there is nothing to replace.
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
