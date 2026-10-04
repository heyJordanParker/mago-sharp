use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum StructureError {
    IoError(io::Error),
    MissingLean(io::Error),
    LeanFailed { file: PathBuf, output: String },
}

impl fmt::Display for StructureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError(error) => write!(f, "IO error: {error}"),
            Self::MissingLean(error) => {
                write!(f, "Cannot run `lean` ({error}). Install Lean 4 with elan and put `lean` on the PATH.")
            }
            Self::LeanFailed { file, output } => write!(f, "Lean failed on `{}`:\n{output}", file.display()),
        }
    }
}

impl std::error::Error for StructureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError(error) | Self::MissingLean(error) => Some(error),
            Self::LeanFailed { .. } => None,
        }
    }
}

impl From<io::Error> for StructureError {
    fn from(error: io::Error) -> Self {
        Self::IoError(error)
    }
}
