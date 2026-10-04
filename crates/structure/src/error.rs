use std::fmt;
use std::io;
use std::path::PathBuf;

use mago_extension::PayloadError;
use mago_extension::WorkerError;

#[derive(Debug)]
pub enum StructureError {
    IoError(io::Error),
    MissingLean(io::Error),
    LeanFailed { file: PathBuf, output: String },
    Runner(WorkerError),
    Protocol(String),
}

impl fmt::Display for StructureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError(error) => write!(f, "IO error: {error}"),
            Self::MissingLean(error) => {
                write!(f, "Cannot run `lean` ({error}). Install Lean 4 with elan and put `lean` on the PATH.")
            }
            Self::LeanFailed { file, output } => write!(f, "Lean failed on `{}`:\n{output}", file.display()),
            Self::Runner(error) => write!(f, "`sharp-rules` failed: {error}"),
            Self::Protocol(message) => write!(f, "`sharp-rules` broke the structure protocol: {message}"),
        }
    }
}

impl std::error::Error for StructureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError(error) | Self::MissingLean(error) => Some(error),
            Self::Runner(error) => Some(error),
            Self::LeanFailed { .. } | Self::Protocol(_) => None,
        }
    }
}

impl From<io::Error> for StructureError {
    fn from(error: io::Error) -> Self {
        Self::IoError(error)
    }
}

impl From<WorkerError> for StructureError {
    fn from(error: WorkerError) -> Self {
        Self::Runner(error)
    }
}

impl From<PayloadError> for StructureError {
    fn from(error: PayloadError) -> Self {
        Self::Protocol(error.to_string())
    }
}
