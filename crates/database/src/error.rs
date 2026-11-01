use std::path::Path;
use std::path::PathBuf;

use globset::Error as GlobSetError;

/// What mago was doing to a path when the file system failed it: loading the database, or reading, writing or deleting
/// a file a command keeps, as `mago compile` does in the `.sharp` folder. The message of an error names it, so it reads
/// alone, with no word about the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IOAction {
    ReadFile,
    WriteFile,
    DeleteFile,
    OpenFolder,
    /// Reading the next entry of a folder that opened. An entry whose type can't be read is `ReadMetadata` on the
    /// entry's own path.
    WalkFolder,
    CreateFolder,
    DeleteFolder,
    /// Finding the real path a path resolves to.
    ResolvePath,
    ReadMetadata,
}

impl std::fmt::Display for IOAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReadFile => "read the file",
            Self::WriteFile => "write the file",
            Self::DeleteFile => "delete the file",
            Self::OpenFolder => "open the folder",
            Self::WalkFolder => "walk the folder",
            Self::CreateFolder => "create the folder",
            Self::DeleteFolder => "delete the folder",
            Self::ResolvePath => "resolve the path",
            Self::ReadMetadata => "read the metadata of",
        })
    }
}

/// The primary error type for all database loading and mutation operations.
///
/// This enum consolidates errors from various sources, including file I/O,
/// pattern matching, and concurrent access issues, into a single, unified type.
#[derive(Debug)]
pub enum DatabaseError {
    /// An attempt was made to access a file that does not exist in the database.
    FileNotFound,
    /// The file system failed `action` on `path`.
    IOError { action: IOAction, path: PathBuf, error: std::io::Error },
    /// The set of user-provided glob patterns could not be compiled into a `GlobSet`.
    InvalidGlobSet(GlobSetError),
    /// An error occurred while processing a glob pattern.
    Glob(String),
    /// The file being loaded into the database is too large to be processed.
    FileTooLarge(PathBuf, usize, usize),
    /// An attempt was made to commit or consume a `ChangeLog` while other
    /// references to it still exist, indicating that other threads may not have
    /// finished their work.
    ChangeLogInUse,
    /// The lock on a `ChangeLog` was "poisoned."
    ///
    /// This happens when a thread panics while holding the lock, leaving the
    /// data in an unrecoverable and potentially inconsistent state.
    PoisonedLogMutex,
    /// Failed to initialize the file system watcher.
    #[cfg(feature = "watcher")]
    WatcherInit(notify::Error),
    /// Failed to add a path to the file system watcher.
    #[cfg(feature = "watcher")]
    WatcherWatch(notify::Error),
    /// Attempted to wait on a watcher that is not currently watching.
    #[cfg(feature = "watcher")]
    WatcherNotActive,
}

impl std::fmt::Display for DatabaseError {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound => write!(f, "file not found in database"),
            Self::IOError { action, path, error } => write!(f, "can't {action} `{}`: {error}", path.display()),
            Self::InvalidGlobSet(err) => write!(f, "failed to build exclusion filter from patterns: {err}"),
            Self::Glob(err) => write!(f, "glob pattern error: {err}"),
            Self::FileTooLarge(path, size, max_size) => {
                write!(
                    f,
                    "file at {} is too large to be processed: {} bytes (maximum is {} bytes)",
                    path.display(),
                    size,
                    max_size
                )
            }
            Self::ChangeLogInUse => {
                write!(f, "cannot commit changelog because it is still in use by another thread")
            }
            Self::PoisonedLogMutex => {
                write!(f, "changelog is in an unrecoverable state because a thread panicked while modifying it")
            }
            #[cfg(feature = "watcher")]
            Self::WatcherInit(err) => write!(f, "failed to initialize file watcher: {err}"),
            #[cfg(feature = "watcher")]
            Self::WatcherWatch(err) => write!(f, "failed to watch path: {err}"),
            #[cfg(feature = "watcher")]
            Self::WatcherNotActive => write!(f, "watcher is not currently watching - call watch() first"),
        }
    }
}

impl std::error::Error for DatabaseError {
    #[inline]
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IOError { error, .. } => Some(error),
            Self::InvalidGlobSet(err) => Some(err),
            #[cfg(feature = "watcher")]
            Self::WatcherInit(err) | Self::WatcherWatch(err) => Some(err),
            _ => None,
        }
    }
}

impl DatabaseError {
    /// The error of the file system failing `action` on `path`. An `std::io::Error` becomes a `DatabaseError` only
    /// here, so each one names the path it failed on.
    #[inline]
    #[must_use]
    pub fn io(action: IOAction, path: &Path, error: std::io::Error) -> Self {
        Self::IOError { action, path: path.to_path_buf(), error }
    }
}

/// Lets code that returns `std::io::Result` fail with a `DatabaseError`, whose message names the path, keeping the kind
/// of the I/O error it holds.
impl From<DatabaseError> for std::io::Error {
    fn from(error: DatabaseError) -> Self {
        let kind = match &error {
            DatabaseError::IOError { error, .. } => error.kind(),
            _ => std::io::ErrorKind::Other,
        };

        std::io::Error::new(kind, error)
    }
}

impl From<GlobSetError> for DatabaseError {
    #[inline]
    fn from(error: GlobSetError) -> Self {
        Self::InvalidGlobSet(error)
    }
}
