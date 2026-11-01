use std::borrow::Cow;
use std::path::PathBuf;

use crate::error::DatabaseError;
use crate::error::IOAction;

#[derive(Debug)]
pub(crate) enum FilesystemOperation {
    Write(PathBuf, Cow<'static, [u8]>),
    Delete(PathBuf),
}

impl FilesystemOperation {
    /// Executes the filesystem operation.
    pub fn execute(self) -> Result<(), DatabaseError> {
        match self {
            Self::Write(path, content) => {
                std::fs::write(&path, content).map_err(|error| DatabaseError::io(IOAction::WriteFile, &path, error))
            }
            Self::Delete(path) => {
                std::fs::remove_file(&path).map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))
            }
        }
    }
}
