use std::io::Read;
use std::path::Path;

use mago_database::error::DatabaseError;
use mago_orchestrator::Orchestrator;

use crate::error::Error;

/// When `--stdin-input` is used: validates exactly one path, reads stdin,
/// computes workspace-relative logical name (with `./` normalization), and returns
/// `Some((logical_name, content))` for `load_database(..., stdin_override)`.
/// Otherwise returns `None`. The caller decides which source paths the file joins.
pub fn resolve_stdin_override(
    stdin_input: bool,
    path: &[std::path::PathBuf],
    workspace: &Path,
) -> Result<Option<(String, Vec<u8>)>, Error> {
    if !stdin_input {
        return Ok(None);
    }
    if path.len() != 1 {
        return Err(Error::Database(DatabaseError::IOError(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "When using --stdin-input, exactly one file path must be provided.",
        ))));
    }
    let path = &path[0];
    // PHP source is binary-safe, so read raw bytes: a buffer piped in may not be valid UTF-8.
    let mut content = Vec::new();
    std::io::stdin().read_to_end(&mut content).map_err(|e| Error::Database(DatabaseError::IOError(e)))?;

    #[cfg(windows)]
    let mut logical_name = path.strip_prefix(workspace).unwrap_or(path.as_path()).to_string_lossy().replace('\\', "/");
    #[cfg(not(windows))]
    let mut logical_name = path.strip_prefix(workspace).unwrap_or(path.as_path()).to_string_lossy().into_owned();
    while logical_name.starts_with("./") {
        logical_name = logical_name.split_off(2);
    }

    Ok(Some((logical_name, content)))
}

/// Sets the orchestrator source paths from the given path list.
/// Call when the path list is non-empty (e.g. after handling a possible
/// `--staged` branch in lint), including the one path `--stdin-input` names.
pub fn set_source_paths_from_paths(orchestrator: &mut Orchestrator, paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    orchestrator.set_source_paths(paths.iter().map(|p| p.to_string_lossy().to_string()));
}
