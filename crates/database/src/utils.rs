use std::borrow::Cow;
#[cfg(feature = "loader")]
use std::ffi::OsStr;
use std::fs::read;
#[cfg(all(unix, feature = "loader"))]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
#[cfg(feature = "loader")]
use std::path::PathBuf;

use crate::error::DatabaseError;
use crate::error::IOAction;
use crate::file::File;
use crate::file::FileType;

/// Borrows `bytes` as an [`OsStr`] for platform-native filesystem APIs.
///
/// On Unix, paths are arbitrary byte sequences; the borrow is direct. On Windows
/// (and other non-Unix platforms), paths are UTF-8 — invalid sequences fall back to
/// lossy decoding with replacement characters.
#[cfg(feature = "loader")]
pub(crate) fn bytes_to_os_str(bytes: &[u8]) -> Cow<'_, OsStr> {
    #[cfg(unix)]
    {
        Cow::Borrowed(OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        match std::str::from_utf8(bytes) {
            Ok(s) => Cow::Borrowed(OsStr::new(s)),
            Err(_) => Cow::Owned(String::from_utf8_lossy(bytes).into_owned().into()),
        }
    }
}

/// Borrows `bytes` as a [`Path`].
#[cfg(feature = "loader")]
pub(crate) fn bytes_to_path(bytes: &[u8]) -> Cow<'_, Path> {
    match bytes_to_os_str(bytes) {
        Cow::Borrowed(s) => Cow::Borrowed(Path::new(s)),
        Cow::Owned(s) => Cow::Owned(PathBuf::from(s)),
    }
}

/// Returns `bytes` as a UTF-8 string, replacing invalid sequences.
#[cfg(feature = "loader")]
#[inline]
pub(crate) fn bytes_to_string_lossy(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// The maximum allowed file size (256 MiB).
const MAXIMUM_FILE_SIZE: usize = 256 * 1024 * 1024;

/// Reads the file at `path` from disk as a [`File`] of `file_type`, as [`file_from`] names it.
///
/// # Errors
///
/// Returns a [`DatabaseError::IOError`] naming `path` if the file cannot be read from the filesystem, and the error of
/// [`file_from`].
pub(crate) fn read_file(workspace: &Path, path: &Path, file_type: FileType) -> Result<File, DatabaseError> {
    let bytes = read(path).map_err(|error| DatabaseError::io(IOAction::ReadFile, path, error))?;

    file_from(workspace, path, file_type, bytes)
}

/// The [`File`] of `file_type` at `path` that holds `bytes`, named by [`File::logical_name`].
///
/// # Errors
///
/// Returns a [`DatabaseError::FileTooLarge`] naming `path` if `bytes` are over [`MAXIMUM_FILE_SIZE`].
pub(crate) fn file_from(
    workspace: &Path,
    path: &Path,
    file_type: FileType,
    bytes: Vec<u8>,
) -> Result<File, DatabaseError> {
    if bytes.len() > MAXIMUM_FILE_SIZE {
        return Err(DatabaseError::FileTooLarge(path.to_path_buf(), bytes.len(), MAXIMUM_FILE_SIZE));
    }

    Ok(File::new(
        Cow::Owned(File::logical_name(workspace, path)),
        file_type,
        Some(path.to_path_buf()),
        Cow::Owned(bytes),
    ))
}
