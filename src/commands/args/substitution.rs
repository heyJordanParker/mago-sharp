//! Command-line arguments for file-content substitution.
//!
//! This module defines [`SubstitutionArgs`], a reusable set of arguments for swapping one or
//! more host files with replacement files while keeping the rest of the project intact. The
//! primary consumer is mutation-testing tooling that wants the analyzer and linter to evaluate
//! a mutated copy of a source file without changing the original on disk.
//!
//! # Syntax
//!
//! Each `--substitute ORIG=TEMP` pair maps an existing host file inside the workspace (`ORIG`)
//! to a file providing replacement content (`TEMP`). Both paths must be absolute. The flag can
//! be given multiple times to substitute several files in a single invocation.
//!
//! # Mechanism
//!
//! The project loads as it would without the flag, and [`substitute`] then puts each `TEMP` in
//! the database in place of `ORIG`, as `mago convert` swaps a converted file in. The replacement
//! is checked exactly as the original would be at the original's path: it takes the original's
//! name, package, folder namespace and exclusions, and keeps its own dialect, from its own
//! extension. Its own location neither excludes it nor adds it as a file of its own.
//!
//! # Validation
//!
//! Argument parsing only checks surface shape (presence of `=`, non-empty segments). Existence
//! and absolute-path checks happen in [`SubstitutionArgs::resolve`] before the orchestrator is
//! handed the mapping.

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;

use clap::Parser;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::loader::mark_sharp_file;

use crate::error::Error;

/// A parsed `--substitute ORIG=TEMP` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    /// Absolute path to the original host file being replaced.
    pub original: PathBuf,
    /// Absolute path to the file providing replacement content.
    pub temporary: PathBuf,
}

/// Command-line arguments for file-content substitution.
///
/// Designed to be flattened into command structs via `#[clap(flatten)]`.
#[derive(Parser, Debug, Clone, Default)]
pub struct SubstitutionArgs {
    /// Replace a host file with another file for this invocation (`ORIG=TEMP`).
    ///
    /// Both paths must be absolute. `ORIG` must be a host file inside the project; `TEMP` is
    /// any readable file providing the replacement content. Repeat the flag to substitute
    /// multiple files in a single run.
    ///
    /// Primarily intended for mutation-testing frameworks that generate a mutated copy of a
    /// source file and want the analyzer or linter to evaluate it against the rest of the
    /// project without writing the mutation to its original location.
    #[arg(
        long = "substitute",
        value_name = "ORIG=TEMP",
        conflicts_with_all = ["stdin_input"]
    )]
    pub substitutions: Vec<String>,
}

impl SubstitutionArgs {
    /// Parses and validates every `--substitute` value.
    ///
    /// Validates: `ORIG=TEMP` shape, absolute paths, existence of both files. Returns the
    /// typed pairs in the order they were given on the command line.
    pub fn resolve(&self) -> Result<Vec<Substitution>, Error> {
        self.substitutions.iter().map(|raw| parse_substitute_pair(raw)).collect()
    }
}

/// Puts each substitution's `temporary` file in `database` in place of its `original`.
///
/// The replacement is named at the original's path, with its own extension when it has one, so
/// its diagnostics name the file it becomes once it's swapped in, and its dialect is its own. The
/// original's folder gives it its package, as the loader marks a file it reads there. A replaced
/// file the database doesn't hold is one the command doesn't check, so its replacement isn't
/// checked either. The replacement keeps its own disk path, where a fix is written.
///
/// # Errors
///
/// Returns an [`Error`] when a replacement can't be read.
pub fn substitute(substitutions: &[Substitution], database: &mut Database<'_>, workspace: &Path) -> Result<(), Error> {
    for substitution in substitutions {
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let original = database.get_by_path(&canonical(&substitution.original));
        if let Ok(own) = database.get_by_path(&canonical(&substitution.temporary)) {
            database.delete(own.id);
        }

        let Ok(original) = original else {
            tracing::warn!(
                "`{}` is not a file this command checks, so its replacement `{}` is not checked either.",
                substitution.original.display(),
                substitution.temporary.display()
            );
            continue;
        };

        let mut name = PathBuf::from(String::from_utf8_lossy(&original.name).into_owned());
        if let Some(extension) = substitution.temporary.extension() {
            name.set_extension(extension);
        }

        let draft = File::read(workspace, &substitution.temporary, original.file_type)?;
        let mut replacement = File::new(
            Cow::Owned(name.to_string_lossy().into_owned().into_bytes()),
            original.file_type,
            None,
            draft.contents,
        );
        mark_sharp_file(&mut replacement, workspace);
        replacement.path = Some(substitution.temporary.clone());

        database.delete(original.id);
        database.add(replacement);
    }

    Ok(())
}

fn parse_substitute_pair(raw: &str) -> Result<Substitution, Error> {
    let (original, temporary) = raw.split_once('=').ok_or_else(|| {
        Error::InvalidArgument(format!("--substitute value `{raw}` is missing `=`; expected format is ORIG=TEMP"))
    })?;

    if original.is_empty() {
        return Err(Error::InvalidArgument(format!("--substitute value `{raw}` is missing the ORIG path before `=`")));
    }

    if temporary.is_empty() {
        return Err(Error::InvalidArgument(format!("--substitute value `{raw}` is missing the TEMP path after `=`")));
    }

    let original = PathBuf::from(original);
    let temporary = PathBuf::from(temporary);

    if !original.is_absolute() {
        return Err(Error::InvalidArgument(format!(
            "--substitute: original path `{}` must be absolute",
            original.display()
        )));
    }

    if !temporary.is_absolute() {
        return Err(Error::InvalidArgument(format!(
            "--substitute: temporary path `{}` must be absolute",
            temporary.display()
        )));
    }

    if !original.exists() {
        return Err(Error::InvalidArgument(format!(
            "--substitute: original path `{}` does not exist",
            original.display()
        )));
    }

    if !temporary.exists() {
        return Err(Error::InvalidArgument(format!(
            "--substitute: temporary path `{}` does not exist",
            temporary.display()
        )));
    }

    Ok(Substitution { original, temporary })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn make_tempfile(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mago-substitute-test-{}.php", uuid_like()));
        fs::write(&path, contents).expect("write tempfile");
        path
    }

    fn uuid_like() -> u128 {
        use std::time::SystemTime;

        SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    }

    #[test]
    fn parses_valid_pair() {
        let orig = make_tempfile("<?php // orig");
        let temp = make_tempfile("<?php // temp");

        let raw = format!("{}={}", orig.display(), temp.display());
        let pair = parse_substitute_pair(&raw).expect("parses");

        assert_eq!(pair.original, orig);
        assert_eq!(pair.temporary, temp);

        let _ = fs::remove_file(&orig);
        let _ = fs::remove_file(&temp);
    }

    #[test]
    fn rejects_missing_equals_sign() {
        let error = parse_substitute_pair("/abs/orig.php").unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
    }

    #[test]
    fn rejects_empty_original() {
        let error = parse_substitute_pair("=/abs/temp.php").unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
    }

    #[test]
    fn rejects_empty_temporary() {
        let error = parse_substitute_pair("/abs/orig.php=").unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
    }

    #[test]
    fn rejects_relative_original() {
        let error = parse_substitute_pair("orig.php=/abs/temp.php").unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
    }

    #[test]
    fn rejects_missing_original_file() {
        let error = parse_substitute_pair("/definitely/does/not/exist.php=/also/not/real.php").unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
    }
}
