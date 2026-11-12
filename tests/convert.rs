//! Runs `mago convert --dry-run` on a project whose git `mago convert` can't read or run.

use std::path::Path;
use std::process::Command;
use std::process::Output;

use mago_database::process;

const CLASS: &str = "<?php\n\nnamespace Generated;\n\ninterface Clean\n{\n    public function id(): int;\n}\n";

const SCRIPT: &str = "<?php\n\nfunction helper(): int\n{\n    return 1;\n}\n";

/// A project at `root` with `src/` the PSR-4 folder of `Generated\`, holding `files`, each a path under `src/` and its
/// code.
fn project(root: &Path, files: &[(&str, &str)]) {
    std::fs::write(root.join("mago.toml"), "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\n").unwrap();
    std::fs::write(root.join("composer.json"), r#"{"autoload": {"psr-4": {"Generated\\": "src/"}}}"#).unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    for (path, code) in files {
        std::fs::write(root.join("src").join(path), code).unwrap();
    }
}

/// `mago convert --dry-run src` in `root`, with `environment` added to the environment.
fn convert(root: &Path, environment: &[(&str, &str)]) -> Output {
    process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "convert", "--dry-run", "src"])
            .env("MAGO_LOG", "info")
            .envs(environment.iter().copied())
            .current_dir(root),
    )
    .unwrap()
}

/// What `output` printed.
fn printed(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Whether `output` logged exactly `message` at `level`, after the timestamp, thread and target each log line starts
/// with.
fn logged(output: &Output, level: &str, message: &str) -> bool {
    printed(output).lines().any(|line| line.contains(&format!(" {level} ")) && line.ends_with(&format!(": {message}")))
}

/// A linked worktree whose `.git` file names a git folder that isn't there, as in a container that mounts the worktree
/// alone, stops the run with exit code 2 and says what to do: mount the main repository's `.git` folder, or run in a
/// plain clone. On 0.6.5 it said "`git rev-parse` failed (exit status: 128): fatal: not a git repository" with the
/// host's path alone, and on 0.6.3 every file stayed plain PHP for "commit it first".
#[test]
fn a_worktree_whose_git_folder_is_missing_says_to_mount_it_or_run_in_a_plain_clone() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    project(&root, &[("Clean.php", CLASS)]);
    std::fs::write(root.join(".git"), "gitdir: /missing/.git/worktrees/design\n").unwrap();

    let output = convert(&root, &[]);

    assert_eq!(output.status.code(), Some(2), "{}", printed(&output));
    let reason = format!(
        "{} names the git folder /missing/.git/worktrees/design, which doesn't exist here, as in a container that mounts a git worktree alone. mago convert converts only files git holds, so mount the main repository's .git folder at the path it has where the worktree was made, so that folder exists, or run mago convert in a plain clone.",
        root.join(".git").display()
    );
    assert!(logged(&output, "ERROR", &reason), "{}", printed(&output));
}

/// A repository with no commit holds no file, which `git rev-parse` tells by its exit code 1 whatever it prints, so
/// each file stays plain PHP for git holding no commit of it, with `GIT_TRACE` on. The run completes, so it exits 0.
#[test]
fn a_repository_with_no_commit_holds_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    project(root, &[("Clean.php", CLASS)]);
    let init = process::output(Command::new("git").args(["init", "--quiet"]).current_dir(root)).unwrap();
    assert!(init.status.success());

    let output = convert(root, &[("GIT_TRACE", "1")]);

    assert_eq!(output.status.code(), Some(0), "{}", printed(&output));
    let reason = "  src/Clean.php: git holds no commit of it as it is: commit it first, so git holds its plain PHP";
    assert!(logged(&output, "WARN", reason), "{}", printed(&output));
}

/// With no `git` to run, the run stops with exit code 2 and says so.
#[test]
fn a_missing_git_stops_the_run_and_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    project(root, &[("Clean.php", CLASS)]);

    let output = convert(root, &[("PATH", "")]);

    assert_eq!(output.status.code(), Some(2), "{}", printed(&output));
    let reason = "Failed to run `git rev-parse`: git isn't installed or isn't on PATH";
    assert!(logged(&output, "ERROR", reason), "{}", printed(&output));
}

/// Shape decides before git, so a script stays plain PHP for being a script, with no `git` to run, and the run
/// completes with exit code 0.
#[test]
fn a_script_stays_plain_php_without_git() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    project(root, &[("helpers.php", SCRIPT)]);

    let output = convert(root, &[("PATH", "")]);

    assert_eq!(output.status.code(), Some(0), "{}", printed(&output));
    let reason = "  src/helpers.php: it declares no class, interface or enum, so it is a script";
    assert!(logged(&output, "WARN", reason), "{}", printed(&output));
}
