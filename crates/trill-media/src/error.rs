//! What can go wrong reading or writing an audio file.

use std::path::{Path, PathBuf};

/// Why an audio file could not be read or written.
///
/// The variants separate the cases a user can act on differently: a file
/// that is gone, one this program cannot read, and one that is damaged.
/// The type is cloneable and comparable so the cache can remember a failure
/// and hand the same one back to every caller.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("'{}' does not exist", .path.display())]
    NotFound { path: PathBuf },
    #[error("cannot read '{}': {reason}", .path.display())]
    Io { path: PathBuf, reason: String },
    #[error("'{}' is not audio this program can read: {reason}", .path.display())]
    Unsupported { path: PathBuf, reason: String },
    #[error("'{}' is damaged: {reason}", .path.display())]
    Corrupt { path: PathBuf, reason: String },
    #[error("'{}' contains no audio", .path.display())]
    Empty { path: PathBuf },
    #[error("cannot write '{}': {reason}", .path.display())]
    Write { path: PathBuf, reason: String },
}

impl MediaError {
    /// The file the error is about.
    pub fn path(&self) -> &Path {
        match self {
            Self::NotFound { path }
            | Self::Io { path, .. }
            | Self::Unsupported { path, .. }
            | Self::Corrupt { path, .. }
            | Self::Empty { path }
            | Self::Write { path, .. } => path,
        }
    }

    pub(crate) fn unsupported(path: &Path, reason: impl Into<String>) -> Self {
        Self::Unsupported { path: path.to_path_buf(), reason: reason.into() }
    }

    pub(crate) fn corrupt(path: &Path, reason: impl Into<String>) -> Self {
        Self::Corrupt { path: path.to_path_buf(), reason: reason.into() }
    }

    pub(crate) fn write(path: &Path, reason: impl ToString) -> Self {
        Self::Write { path: path.to_path_buf(), reason: reason.to_string() }
    }

    /// An error from opening or reading the file itself.
    pub(crate) fn from_io(path: &Path, error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound { path: path.to_path_buf() },
            _ => Self::Io { path: path.to_path_buf(), reason: error.to_string() },
        }
    }
}
