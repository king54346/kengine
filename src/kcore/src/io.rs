//! File loading. Provides an async API so that file access can be made uniform
//! across native targets and the web, where synchronous file access is impossible.

use std::{
    fmt::{Display, Formatter},
    path::Path,
};

/// An error that may occur while loading a file.
#[derive(Debug)]
pub enum FileError {
    /// An i/o error occurred.
    Io(std::io::Error),
    /// A platform-specific error occurred.
    Custom(String),
}

impl Display for FileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Io(err) => Display::fmt(err, f),
            FileError::Custom(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FileError::Io(err) => Some(err),
            FileError::Custom(_) => None,
        }
    }
}

impl From<std::io::Error> for FileError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Reads the entire contents of the file at the given path.
pub async fn load_file<P: AsRef<Path>>(path: P) -> Result<Vec<u8>, FileError> {
    Ok(std::fs::read(path)?)
}

/// Returns `true` if the given path points at an existing entity.
pub fn exists<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().exists()
}

/// Returns `true` if the given path points at a directory.
pub fn is_dir<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().is_dir()
}

/// Returns `true` if the given path points at a regular file.
pub fn is_file<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().is_file()
}
