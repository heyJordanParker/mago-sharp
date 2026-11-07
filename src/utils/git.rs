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
use mago_database::process;

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

/// What git's last commit holds under a workspace, each path relative to it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Committed {
    /// The files the last commit holds with no change since, staged or not. A file git is told to assume unchanged, or
    /// to skip in the worktree, is left out, since git shows none of its changes.
    pub files: HashSet<PathBuf>,
    /// The submodules the last commit holds, whose files another repository holds.
    pub submodules: Vec<PathBuf>,
}

/// What git's last commit holds under `workspace`. Nothing in a repository with no commit.
///
/// # Errors
///
/// Returns [`Error::RunningGit`] when git can't start, and [`Error::GitFailed`] with git's reason when it can't read
/// the repository: outside one, or in a linked worktree whose git folder isn't there, as in a container that mounts
/// the worktree alone.
pub fn committed_files(workspace: &Path) -> Result<Committed, Error> {
    let head =
        process::output(Command::new("git").args(["rev-parse", "--verify", "--quiet", "HEAD"]).current_dir(workspace))
            .map_err(|source| Error::RunningGit { command: "rev-parse", source })?;
    // `--verify --quiet` exits 1 when `HEAD` names no commit, and git exits 128 when it can't read the repository.
    if head.status.code() == Some(1) {
        return Ok(Committed::default());
    }
    succeeded("rev-parse", head)?;
    let entries = |output: Output| -> Vec<String> {
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .map(|entry| String::from_utf8_lossy(entry).into_owned())
            .collect()
    };
    let listed = run_git("ls-tree", Command::new("git").args(["ls-tree", "-r", "-z", "HEAD"]).current_dir(workspace))?;
    let changed = run_git(
        "diff",
        Command::new("git").args(["diff", "-z", "--name-only", "--relative", "HEAD"]).current_dir(workspace),
    )?;
    let tagged = run_git("ls-files", Command::new("git").args(["ls-files", "-v", "-z"]).current_dir(workspace))?;
    // `ls-files -v` tags a file git assumes unchanged with a lowercase letter, and one it skips in the worktree with `S`.
    let mut unchecked: HashSet<String> = entries(tagged)
        .into_iter()
        .filter_map(|entry| {
            let (tag, path) = entry.split_once(' ')?;
            tag.chars().any(|tag| tag.is_ascii_lowercase() || tag == 'S').then(|| path.to_owned())
        })
        .collect();
    unchecked.extend(entries(changed));

    let mut committed = Committed::default();
    // Each entry of `ls-tree` is `<mode> <type> <object>`, a tab, and the path.
    for entry in entries(listed) {
        let Some((object, path)) = entry.split_once('\t') else {
            continue;
        };
        if object.starts_with("160000 ") {
            committed.submodules.push(PathBuf::from(path));
        } else if !unchecked.contains(path) {
            committed.files.insert(PathBuf::from(path));
        }
    }

    Ok(committed)
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

/// Removes from the `info/exclude` file git reads for `workspace` every block `owner` wrote there, whichever folder it
/// names. `owner` kept its files out of git in those blocks, but every checkout of a repository shares the file, so one
/// checkout's block hid the same paths in every other checkout. It is for checkouts upgraded from mago 0.6.3 or
/// earlier, which wrote the blocks.
///
/// A block is a heading, the lines right after it that start with `/`, and the end line when it comes next, so a block
/// torn before its end line loses only its own `/` lines. Every other byte stays, a CRLF line end, a byte that is no
/// UTF-8 and a last line with no line end included. [`exclude_file`] names the file, with or without a `git` to run.
/// The file is replaced by rename, so a compile that reads it while another writes it never reads part of it. A file
/// with no block is left as it is. One that can't be written keeps its blocks, and the compile warns.
pub fn remove_exclude_blocks(workspace: &Path, owner: &str) -> Result<(), Error> {
    let Some(file) = exclude_file(workspace) else {
        return Ok(());
    };
    let before = match std::fs::read(&file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DatabaseError::io(IOAction::ReadFile, &file, error).into()),
    };
    let heading = format!("# {owner}: the files it writes in ");
    let end = format!("# {owner}: end");
    let mut after = Vec::with_capacity(before.len());
    let mut in_block = false;
    for line in before.split_inclusive(|byte| *byte == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if in_block && text.starts_with(b"/") {
            continue;
        }
        if in_block && text == end.as_bytes() {
            in_block = false;
            continue;
        }
        in_block = text.starts_with(heading.as_bytes());
        if !in_block {
            after.extend_from_slice(line);
        }
    }
    if after == before {
        return Ok(());
    }
    if let Err(error) = write_by_rename(&file, &after) {
        // The new file is written beside the old one and renamed over it, so it is the folder that must be writable,
        // and the error's own path names the temporary file.
        let folder = file.parent().unwrap_or(&file).display();
        tracing::warn!(
            "{} can't be replaced, as {folder} can't be written: {}, so its {owner} blocks still hide their files in every checkout of the repository. Make {folder} writable, and run vendor/bin/mago compile again.",
            file.display(),
            std::io::Error::from(error).kind()
        );
    }

    Ok(())
}

/// The `info/exclude` file git reads for the repository that holds `workspace`, found as git finds it: the nearest
/// `.git` is the repository's folder, or a file that names it, as a linked worktree's and a submodule's is, and a
/// `commondir` there names the folder every checkout of the repository shares. Reading the files needs no `git` to
/// run, as libgit2 and JGit read a repository. None outside a git repository.
fn exclude_file(workspace: &Path) -> Option<PathBuf> {
    let top = workspace.ancestors().find(|folder| folder.join(".git").exists())?;
    let dot_git = top.join(".git");
    let folder = if dot_git.is_dir() {
        dot_git
    } else {
        let contents = std::fs::read_to_string(&dot_git).unwrap_or_default();
        top.join(contents.strip_prefix("gitdir:").unwrap_or(&contents).trim())
    };
    let shared = match std::fs::read_to_string(folder.join("commondir")) {
        Ok(common) => folder.join(common.trim()),
        Err(_) => folder,
    };

    Some(shared.join("info").join("exclude"))
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
    process::output(Command::new("git").args(["rev-parse", "--git-dir"]).current_dir(workspace))
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
    let mut child = process::spawn(
        Command::new("git")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(["hash-object", "-w", "--stdin", "--path"])
            .arg(path)
            .current_dir(workspace),
    )
    .map_err(|source| Error::RunningGit { command: "hash-object", source })?;

    let mut stdin = child.stdin.take().expect("failed to get stdin");
    std::thread::spawn(move || {
        mago_extension::process_group::block_sigpipe(&stdin);
        // A git that exits before it reads the content closes the pipe, and its exit status reports why.
        let _result = stdin.write_all(&content);
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
    let output = process::output(git).map_err(|source| Error::RunningGit { command, source })?;

    succeeded(command, output)
}

/// The `output` of the `git` subcommand `command`, or [`Error::GitFailed`] with git's [`reason`] when it exited with an
/// error.
fn succeeded(command: &'static str, output: Output) -> Result<Output, Error> {
    if output.status.success() {
        return Ok(output);
    }

    Err(Error::GitFailed { command, status: output.status, reason: reason(&output.stderr) })
}

/// Why git failed, from what it printed to `stderr`: its first `fatal:` or `error:` line, past the lines `GIT_TRACE`
/// adds, or its first line when it printed neither. The lines after its reason are advice for its own command line.
fn reason(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let mut lines = stderr.lines().map(str::trim);
    let first = lines.clone().next().unwrap_or_default();

    lines.find(|line| line.starts_with("fatal:") || line.starts_with("error:")).unwrap_or(first).to_owned()
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

    /// Git holds a file of the last commit that has no change since, and neither an untracked file, a staged one, one
    /// changed on disk, nor one git is told to assume unchanged or to skip, whose changes it never shows.
    #[test]
    fn the_committed_files_are_those_of_the_last_commit_with_no_change_since() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for name in ["Order.php", "Changed.php", "Staged.php", "Assumed.php", "Skipped.php"] {
            std::fs::write(root.join(name), "<?php\n").unwrap();
        }
        git(root, &["init", "--quiet"]);
        assert_eq!(committed_files(root).unwrap(), Committed::default());
        git(root, &["add", "--all"]);
        git(root, &["commit", "--quiet", "-m", "plain PHP"]);
        std::fs::write(root.join("Changed.php"), "<?php\n// changed\n").unwrap();
        std::fs::write(root.join("Staged.php"), "<?php\n// staged\n").unwrap();
        git(root, &["add", "Staged.php"]);
        std::fs::write(root.join("Draft.php"), "<?php\n").unwrap();
        git(root, &["update-index", "--assume-unchanged", "Assumed.php"]);
        git(root, &["update-index", "--skip-worktree", "Skipped.php"]);
        std::fs::write(root.join("Assumed.php"), "<?php\n// hidden from git\n").unwrap();
        std::fs::write(root.join("Skipped.php"), "<?php\n// hidden from git\n").unwrap();

        assert_eq!(committed_files(root).unwrap().files, HashSet::from([PathBuf::from("Order.php")]));
    }

    /// A submodule's files are another repository's, so the last commit holds the submodule and none of its files.
    #[test]
    fn a_submodule_is_held_as_a_submodule_and_none_of_its_files() {
        let directory = tempfile::tempdir().unwrap();
        let (library, root) = (directory.path().join("library"), directory.path().join("project"));
        for folder in [&library, &root] {
            std::fs::create_dir(folder).unwrap();
            std::fs::write(folder.join("Order.php"), "<?php\n").unwrap();
            git(folder, &["init", "--quiet"]);
            git(folder, &["add", "--all"]);
            git(folder, &["commit", "--quiet", "-m", "plain PHP"]);
        }
        let source = library.to_string_lossy().into_owned();
        git(&root, &["-c", "protocol.file.allow=always", "submodule", "add", "--quiet", &source, "vendor/library"]);
        git(&root, &["commit", "--quiet", "-m", "the library"]);

        let committed = committed_files(&root).unwrap();

        assert_eq!(committed.submodules, [PathBuf::from("vendor/library")]);
        assert_eq!(committed.files, HashSet::from([PathBuf::from("Order.php"), PathBuf::from(".gitmodules")]));
    }

    /// Git's reason is its first `fatal:` or `error:` line, past the trace `GIT_TRACE` prints before it.
    #[test]
    fn gits_reason_is_its_fatal_line_past_its_trace() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join(".git"), "gitdir: /missing/.git/worktrees/design\n").unwrap();
        let rev_parse = |trace: &str| {
            Command::new("git")
                .args(["rev-parse", "--verify", "--quiet", "HEAD"])
                .env("GIT_TRACE", trace)
                .current_dir(root)
                .output()
                .unwrap()
        };
        // Git words its reason by its version, as `not a git repository` or `gitfile does not point to a valid
        // repository`, so the reason is the `fatal:` line git prints with no trace.
        let untraced = rev_parse("0");
        let untraced = String::from_utf8_lossy(&untraced.stderr);
        let alone = untraced.lines().find(|line| line.starts_with("fatal: ")).expect(&untraced);
        let traced = rev_parse("1");
        assert!(String::from_utf8_lossy(&traced.stderr).starts_with(|first: char| first.is_ascii_digit()));

        assert_eq!(reason(&traced.stderr), alone);
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
