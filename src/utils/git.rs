//! Git utilities for staged file operations.
//!
//! This module provides helper functions for interacting with git repositories,
//! specifically for the `--staged` formatting feature that allows formatting
//! staged files in pre-commit hooks.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;

use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::File;
use mago_database::file::FileId;

use crate::error::Error;
use crate::utils::write_by_rename;

/// Get staged file paths relative to the workspace.
///
/// This function is used by `--staged` flags in lint and analyze commands
/// to filter analysis to only staged files.
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
///
/// # Returns
///
/// A vector of staged file paths (relative to workspace), or an error if
/// not in a git repository.
pub fn get_staged_file_paths(workspace: &Path) -> Result<Vec<PathBuf>, Error> {
    if !is_git_repository(workspace) {
        return Err(Error::NotAGitRepository);
    }

    get_staged_files(workspace)
}

/// Creates an ephemeral file with the contents of a staged file
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
/// * `path` - The path for which to get the file
///
/// # Returns
///
/// An ephemeral file with the contents of the staged file
pub fn get_staged_file(workspace: &Path, path: &Path) -> Result<File, Error> {
    let mut index_path = OsString::from(":");
    index_path.push(path);

    let output =
        run_git("cat-file", Command::new("git").args(["cat-file", "-p"]).arg(index_path).current_dir(workspace))?;

    Ok(File::ephemeral(Cow::Borrowed(b"<stdin>"), Cow::Owned(output.stdout)))
}

/// Updates the contents of the staged file
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
/// * `path` - The path for which to get the file
/// * `new_content` - The new content for the staged file
pub fn update_staged_file(workspace: &Path, path: &Path, new_content: Vec<u8>) -> Result<(), Error> {
    let blob_id = create_blob(workspace, path, new_content)?;
    let mode = get_mode(workspace, path)?;

    let mut cacheinfo = OsString::new();
    cacheinfo.push(&mode);
    cacheinfo.push(",");
    cacheinfo.push(&blob_id);
    cacheinfo.push(",");
    cacheinfo.push(path.as_os_str());

    run_git(
        "update-index",
        Command::new("git").args(["update-index", "--cacheinfo"]).arg(cacheinfo).current_dir(workspace),
    )?;

    Ok(())
}

/// Verify that none of the given staged files have unstaged changes.
///
/// This check prevents data loss when applying fixes to staged files:
/// if a file has both staged and unstaged changes, modifying it on disk
/// and re-staging would include the previously-unstaged changes in the commit.
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
/// * `staged_files` - The list of staged file paths to check
///
/// # Returns
///
/// `Ok(())` if all staged files are clean, or `Err(Error::StagedFileHasUnstagedChanges)`
/// if any staged file also has unstaged modifications.
pub fn ensure_staged_files_are_clean(workspace: &Path, staged_files: &[PathBuf]) -> Result<(), Error> {
    let files_with_unstaged = get_files_with_unstaged_changes(workspace)?;

    for staged_file in staged_files {
        if files_with_unstaged.contains(staged_file) {
            return Err(Error::StagedFileHasUnstagedChanges(staged_file.display().to_string()));
        }
    }

    Ok(())
}

/// Stage multiple files at once by their file IDs.
///
/// This function looks up file paths from the database and runs
/// `git add -- <files...>` to stage all specified files in a single git invocation.
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
/// * `database` - The database to look up file paths from
/// * `file_ids` - Iterator of file IDs to stage
///
/// # Returns
///
/// `Ok(())` on success, or an error if the git command fails.
pub fn stage_files<I>(workspace: &Path, database: &Database, file_ids: I) -> Result<(), Error>
where
    I: IntoIterator<Item = FileId>,
{
    let paths: Vec<PathBuf> = file_ids
        .into_iter()
        .filter_map(|id| database.get_ref(&id).ok())
        .map(|file| PathBuf::from(std::str::from_utf8(&file.name).unwrap_or("")))
        .collect();

    if paths.is_empty() {
        return Ok(());
    }

    let mut cmd = Command::new("git");
    cmd.args(["add", "--"]);
    for path in &paths {
        cmd.arg(path);
    }

    run_git("add", cmd.current_dir(workspace))?;

    Ok(())
}

/// The files under `workspace` git holds as they are, each relative to `workspace`: in the last commit, with no change
/// since, staged or not. None outside a git repository, or in one with no commit.
pub fn committed_files(workspace: &Path) -> Result<HashSet<PathBuf>, Error> {
    if !is_git_repository(workspace) {
        return Ok(HashSet::new());
    }
    let paths = |output: Output| -> HashSet<PathBuf> {
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(String::from_utf8_lossy(path).as_ref()))
            .collect()
    };
    // A repository with no commit has no `HEAD` to list.
    let listed = Command::new("git")
        .args(["ls-tree", "-r", "-z", "--name-only", "HEAD"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|output| output.status.success());
    let Some(listed) = listed else {
        return Ok(HashSet::new());
    };
    let changed = run_git(
        "diff",
        Command::new("git").args(["diff", "-z", "--name-only", "--relative", "HEAD"]).current_dir(workspace),
    )?;
    let changed = paths(changed);

    Ok(paths(listed).into_iter().filter(|path| !changed.contains(path)).collect())
}

/// Records in the index the rename of the tracked file `from` to `to`, both relative to `workspace`, with `from`'s
/// contents in the index, as `git mv` records it, and leaves both files on disk as they are. The next commit is then
/// the rename alone, which `git log --follow` follows, and what the caller writes to `to` shows as unstaged.
pub fn stage_rename(workspace: &Path, from: &Path, to: &Path) -> Result<(), Error> {
    let listed = run_git(
        "ls-files",
        Command::new("git")
            .args(["ls-files", "--format=%(objectmode),%(objectname)", "--"])
            .arg(from)
            .current_dir(workspace),
    )?;
    let mut cacheinfo = OsString::from(String::from_utf8_lossy(&listed.stdout).trim());
    cacheinfo.push(",");
    cacheinfo.push(to.as_os_str());
    run_git(
        "update-index",
        Command::new("git").args(["update-index", "--add", "--cacheinfo"]).arg(cacheinfo).current_dir(workspace),
    )?;
    run_git(
        "update-index",
        Command::new("git").args(["update-index", "--force-remove", "--"]).arg(from).current_dir(workspace),
    )?;

    Ok(())
}

/// Keeps `paths`, each a file under `workspace`, out of git: the ones no `.gitignore` already ignores become the block
/// of the repository's `info/exclude` that `owner` holds for `workspace`. A workspace git ignores, as a build folder,
/// has no block, and neither has one whose paths are all ignored. Every other line of the file stays as it is, except
/// the blocks of `owner` for a folder that is gone or that git now ignores, which keep nothing out of git.
///
/// [`repository`] names the file, so a worktree and a submodule write the one git reads for them, with or without a
/// `git` to run. The worktrees of a repository share that file, so each workspace keeps a block of its own, named by its
/// path, and one worktree's compile never drops another's paths. The file is replaced by rename, so a compile that reads
/// it while another writes it never reads part of it. With no `git` to run, or one that fails there, no `.gitignore` is
/// read, so the block lists every path. A workspace outside a git repository writes nothing. One whose `.git` names a
/// missing folder, or whose file can't be written, writes nothing and warns that git shows `paths` as untracked.
pub fn exclude(workspace: &Path, owner: &str, paths: &[PathBuf]) -> Result<(), Error> {
    let (top, file) = match repository(workspace) {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(()),
        Err((dot_git, missing)) => {
            warn_untracked(
                workspace,
                paths,
                &format!("{} names {}, which is missing", dot_git.display(), missing.display()),
                "Make the repository's .git folder reachable here",
            );
            return Ok(());
        }
    };
    let top = top.as_path();
    let heading = format!("# {owner}: the files it writes in ");
    let end = format!("# {owner}: end");

    let before = match std::fs::read_to_string(&file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(DatabaseError::io(IOAction::ReadFile, &file, error).into()),
    };
    let mut after = String::new();
    let mut kept = true;
    for line in before.lines() {
        if let Some(folder) = line.strip_prefix(&heading) {
            let folder = Path::new(folder);
            kept = folder != workspace && folder.is_dir() && !ignores(folder, folder);
        }
        if kept {
            after.push_str(line);
            after.push('\n');
        }
        kept |= line == end;
    }
    let mut block = String::new();
    if !paths.is_empty() && !ignores(workspace, workspace) {
        let ignored = ignored_by_gitignore(workspace);
        for path in paths {
            let (Ok(relative), Ok(local)) = (path.strip_prefix(top), path.strip_prefix(workspace)) else {
                continue;
            };
            if !ignored.iter().any(|entry| local.starts_with(entry)) {
                block.push_str(&pattern(relative));
                block.push('\n');
            }
        }
    }
    if !block.is_empty() {
        after.push_str(&format!("{heading}{}\n{block}{end}\n", workspace.display()));
    }
    if after != before
        && let Err(error) = write_by_rename(&file, after.as_bytes())
    {
        // A file an earlier block lists stays out of git. The new file is written beside the old one and renamed over it,
        // so it is the folder that must be writable, and the error's own path names the temporary file.
        let untracked: Vec<PathBuf> = paths
            .iter()
            .filter(|path| {
                path.strip_prefix(top).is_ok_and(|relative| !before.lines().any(|line| line == pattern(relative)))
            })
            .cloned()
            .collect();
        let folder = file.parent().unwrap_or(&file).display();
        warn_untracked(
            workspace,
            &untracked,
            &format!(
                "{} can't be replaced, as {folder} can't be written: {}",
                file.display(),
                std::io::Error::from(error).kind()
            ),
            &format!("Make {folder} writable"),
        );
    }

    Ok(())
}

/// Warns that git shows `paths`, files under `workspace`, as untracked because of `cause`, and that `fix` and another
/// compile hide them.
fn warn_untracked(workspace: &Path, paths: &[PathBuf], cause: &str, fix: &str) {
    if paths.is_empty() {
        return;
    }
    let listed: Vec<String> =
        paths.iter().map(|path| path.strip_prefix(workspace).unwrap_or(path).display().to_string()).collect();
    tracing::warn!(
        "{cause}, so git shows {} as untracked. {fix}, and run vendor/bin/mago compile again.",
        listed.join(", ")
    );
}

/// The top folder of the git repository that holds `workspace`, and the `info/exclude` file git reads for it, found as
/// git finds them: the nearest `.git` is the repository's folder, or a file that names it, as a linked worktree's and
/// a submodule's is, and a `commondir` there names the folder every checkout of the repository shares. Reading the
/// files needs no `git` to run, as libgit2 and JGit read a repository. `Err` holds a `.git` file and the missing folder
/// it names.
fn repository(workspace: &Path) -> Result<Option<(PathBuf, PathBuf)>, (PathBuf, PathBuf)> {
    let Some(top) = workspace.ancestors().find(|folder| folder.join(".git").exists()) else {
        return Ok(None);
    };
    let dot_git = top.join(".git");
    let folder = if dot_git.is_dir() {
        dot_git
    } else {
        let contents = std::fs::read_to_string(&dot_git).unwrap_or_default();
        let named = top.join(contents.strip_prefix("gitdir:").unwrap_or(&contents).trim());
        if !named.is_dir() {
            return Err((dot_git, named));
        }
        named
    };
    let shared = match std::fs::read_to_string(folder.join("commondir")) {
        Ok(common) => folder.join(common.trim()),
        Err(_) => folder,
    };

    Ok(Some((top.to_path_buf(), shared.join("info").join("exclude"))))
}

/// Whether git, run in `folder`, ignores `path`. A git that fails there ignores nothing, and [`ignored_by_gitignore`]
/// gives its reason.
fn ignores(folder: &Path, path: &Path) -> bool {
    Command::new("git")
        .args(["check-ignore", "--quiet", "--"])
        .arg(path)
        .current_dir(folder)
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The untracked files and folders under `workspace` that its repository's `.gitignore` files ignore, relative to it.
/// `info/exclude` is not read, so the block [`exclude`] writes there never counts. A workspace git ignores as a whole
/// is not listed: git refuses to list from inside one. With no `git` to run, none is listed. When git fails, as in a
/// repository another user owns, none is listed either, and the warning gives git's reason, as cargo warns and goes on
/// when it can't read a repository's status.
fn ignored_by_gitignore(workspace: &Path) -> Vec<PathBuf> {
    let reason = match Command::new("git")
        .args(["ls-files", "-z", "--others", "--ignored", "--directory", "--exclude-per-directory=.gitignore"])
        .current_dir(workspace)
        .output()
    {
        Ok(output) if output.status.success() => {
            return output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).as_ref()))
                .collect();
        }
        // Git's first line is its reason, and the lines after it are advice for its own command line.
        Ok(output) => String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or_default().trim().to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => error.to_string(),
    };
    tracing::warn!(
        "git can't read the .gitignore files of {}: {reason}. The repository's info/exclude lists every file mago compile writes there.",
        workspace.display()
    );

    Vec::new()
}

/// The gitignore pattern that names exactly the path `relative` to the repository's top folder: anchored with `/`,
/// with `\` before each character a pattern reads as a wildcard and before a trailing space.
fn pattern(relative: &Path) -> String {
    let mut pattern = String::new();
    for part in relative.components() {
        pattern.push('/');
        for character in part.as_os_str().to_string_lossy().chars() {
            if matches!(character, '\\' | '*' | '?' | '[') {
                pattern.push('\\');
            }
            pattern.push(character);
        }
    }
    if pattern.ends_with(' ') {
        pattern.insert(pattern.len() - 1, '\\');
    }

    pattern
}

/// Check if we're inside a git repository.
///
/// This function runs `git rev-parse --git-dir` to determine if the given
/// workspace is inside a git repository.
///
/// # Arguments
///
/// * `workspace` - The directory to check
///
/// # Returns
///
/// `true` if the workspace is inside a git repository, `false` otherwise.
fn is_git_repository(workspace: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(workspace)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get list of staged files (returns paths relative to workspace).
///
/// This function runs `git diff --cached --name-only --diff-filter=ACMR` to get
/// the list of files that are staged for commit. The filter excludes deleted files.
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
///
/// # Returns
///
/// A vector of paths relative to the workspace, or an error if git command fails.
fn get_staged_files(workspace: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = run_git(
        "diff",
        Command::new("git").args(["diff", "--cached", "--name-only", "--diff-filter=ACMR"]).current_dir(workspace),
    )?;

    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
}

/// Get set of all files with unstaged changes.
///
/// This function runs `git diff --name-only` once to get all files with unstaged
/// modifications, returning them as a HashSet for O(1) lookup.
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
///
/// # Returns
///
/// A HashSet of paths (relative to workspace) that have unstaged changes.
fn get_files_with_unstaged_changes(workspace: &Path) -> Result<HashSet<PathBuf>, Error> {
    let output = run_git("diff", Command::new("git").args(["diff", "--name-only"]).current_dir(workspace))?;

    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
}

/// Creates a new blob in the git object store for the given contents
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
/// * `path` - The path of the file for which the contents are meant, used to apply git filters
/// * `content` - The contents for the blob object
///
/// # Returns
///
/// A string containing the id of the newly created blob
fn create_blob(workspace: &Path, path: &Path, content: Vec<u8>) -> Result<String, Error> {
    let mut child = Command::new("git")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(["hash-object", "-w", "--stdin", "--path"])
        .arg(path)
        .current_dir(workspace)
        .spawn()
        .map_err(|source| Error::RunningGit { command: "hash-object", source })?;

    let mut stdin = child.stdin.take().expect("failed to get stdin");
    std::thread::spawn(move || {
        stdin.write_all(&content).expect("failed to write to stdin");
    });

    let output = child.wait_with_output().map_err(|source| Error::RunningGit { command: "hash-object", source })?;
    let output = succeeded("hash-object", output)?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Gets the object mode for the given path in the git index
///
/// # Arguments
///
/// * `workspace` - The git repository root directory
/// * `path` - The path for which the object mode is requested
///
/// # Returns
///
/// A string containing the object mode for the path
fn get_mode(workspace: &Path, path: &Path) -> Result<String, Error> {
    let output = run_git(
        "ls-files",
        Command::new("git").args(["ls-files", "--format=%(objectmode)"]).arg(path).current_dir(workspace),
    )?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs `git`, a command that runs the `git` subcommand `command`, and returns its output.
///
/// # Errors
///
/// Returns [`Error::RunningGit`] when git can't start, and [`Error::GitFailed`] when it exits with an error.
fn run_git(command: &'static str, git: &mut Command) -> Result<Output, Error> {
    let output = git.output().map_err(|source| Error::RunningGit { command, source })?;

    succeeded(command, output)
}

/// The `output` of the `git` subcommand `command`, or [`Error::GitFailed`] with the first line git printed to stderr
/// when it exited with an error, as `ignored_by_gitignore` reads git's reason.
fn succeeded(command: &'static str, output: Output) -> Result<Output, Error> {
    if output.status.success() {
        return Ok(output);
    }

    let reason = String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or_default().trim().to_string();

    Err(Error::GitFailed { command, status: output.status, reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(workspace: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=test@localhost"])
            .args(arguments)
            .current_dir(workspace)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Git holds a file of the last commit that has no change since, and neither an untracked file, a staged one nor
    /// one changed on disk.
    #[test]
    fn the_committed_files_are_those_of_the_last_commit_with_no_change_since() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        assert!(committed_files(root).unwrap().is_empty());
        for name in ["Order.php", "Changed.php", "Staged.php"] {
            std::fs::write(root.join(name), "<?php\n").unwrap();
        }
        git(root, &["init", "--quiet"]);
        assert!(committed_files(root).unwrap().is_empty());
        git(root, &["add", "--all"]);
        git(root, &["commit", "--quiet", "-m", "plain PHP"]);
        std::fs::write(root.join("Changed.php"), "<?php\n// changed\n").unwrap();
        std::fs::write(root.join("Staged.php"), "<?php\n// staged\n").unwrap();
        git(root, &["add", "Staged.php"]);
        std::fs::write(root.join("Draft.php"), "<?php\n").unwrap();

        assert_eq!(committed_files(root).unwrap(), HashSet::from([PathBuf::from("Order.php")]));
    }

    /// The index records a rename of the plain file's contents, so it holds the rename alone, while the file written
    /// at the new name shows as an unstaged change.
    #[test]
    fn a_staged_rename_holds_the_old_contents_and_the_written_file_is_unstaged() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("Order.php"), "<?php\n\nfinal class Order {}\n").unwrap();
        git(root, &["init", "--quiet"]);
        git(root, &["add", "Order.php"]);
        git(root, &["commit", "--quiet", "-m", "plain PHP"]);
        std::fs::write(root.join("Order.sharp"), "public final class Order { }\n").unwrap();

        stage_rename(root, Path::new("Order.php"), Path::new("Order.sharp")).unwrap();
        std::fs::remove_file(root.join("Order.php")).unwrap();

        assert_eq!(git(root, &["diff", "--cached", "-M", "--name-status"]), "R100\tOrder.php\tOrder.sharp\n");
        assert_eq!(git(root, &["diff", "--name-only"]), "Order.sharp\n");
        assert_eq!(git(root, &["status", "--porcelain"]), "RM Order.php -> Order.sharp\n");
    }
}
