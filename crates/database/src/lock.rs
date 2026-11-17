//! The lock of a file that Mago's processes and threads take turns on, as `flock` holds it.
//!
//! A lock lives on the open file, so two opens of one file, in one process or in two, lock each other out, and the
//! kernel frees the lock of a process that dies. This crate is the lowest one every crate that takes such a lock
//! depends on.

use std::fs::File;
use std::path::Path;

use crate::error::DatabaseError;
use crate::error::IOAction;

/// Opens the file at `path`, creating it and its folder, and waits until it holds the exclusive lock, which no other
/// open of the file holds at once. The lock lasts until the file is dropped or unlocked.
///
/// # Errors
///
/// Returns the error of creating the folder, or of opening or locking the file.
pub fn exclusive(path: &Path) -> Result<File, DatabaseError> {
    let file = open(path)?;
    file.lock().map_err(|error| DatabaseError::io(IOAction::WriteFile, path, error))?;

    Ok(file)
}

/// Opens the file at `path`, creating it and its folder, and waits until it holds the shared lock.
///
/// Other opens of the file may hold the shared lock at once, and none holds the exclusive one. The lock lasts until
/// the file is dropped or unlocked.
///
/// # Errors
///
/// Returns the error of creating the folder, or of opening or locking the file.
pub fn shared(path: &Path) -> Result<File, DatabaseError> {
    let file = open(path)?;
    file.lock_shared().map_err(|error| DatabaseError::io(IOAction::WriteFile, path, error))?;

    Ok(file)
}

/// The file at `path`, created with its folder when missing, open for writing and never truncated.
fn open(path: &Path) -> Result<File, DatabaseError> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|error| DatabaseError::io(IOAction::CreateFolder, folder, error))?;
    }

    File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|error| DatabaseError::io(IOAction::WriteFile, path, error))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::fs::TryLockError;

    use super::exclusive;
    use super::shared;

    /// Two shared locks of one file hold at once, and no exclusive lock holds beside them, here in one process, whose
    /// opens lock each other out as two processes' do. An exclusive lock holds alone. The file and its folder are
    /// created on the first lock.
    #[test]
    fn shared_locks_hold_together_and_an_exclusive_lock_holds_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("folder/compile.lock");
        let other = || std::fs::File::options().write(true).open(&path).unwrap();

        let readers = (shared(&path).unwrap(), shared(&path).unwrap());
        assert!(matches!(other().try_lock(), Err(TryLockError::WouldBlock)));
        drop(readers);

        let writer = exclusive(&path).unwrap();
        assert!(matches!(other().try_lock_shared(), Err(TryLockError::WouldBlock)));
        drop(writer);
        other().try_lock().unwrap();
    }
}
