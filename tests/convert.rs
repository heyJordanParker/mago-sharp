//! Runs `mago convert --dry-run` on a project whose git `mago convert` can't read or run.

use std::path::Path;
use std::process::Command;
use std::process::Output;

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
    Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "convert", "--dry-run", "src"])
        .env("MAGO_LOG", "info")
        .envs(environment.iter().copied())
        .current_dir(root)
        .output()
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

/// Git that can't read the repository, here a linked worktree whose git folder a container doesn't mount, stops the
/// run with exit code 2, the git command and git's own reason. `GIT_TRACE` prints trace lines before the reason, so
/// the reason is git's `fatal:` line, never its first line. On 0.6.3 every file stayed plain PHP for "commit it first".
#[test]
fn a_repository_git_cannot_read_stops_the_run_with_gits_reason() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    project(root, &[("Clean.php", CLASS)]);
    std::fs::write(root.join(".git"), "gitdir: /missing/.git/worktrees/design\n").unwrap();

    let output = convert(root, &[("GIT_TRACE", "1")]);

    assert_eq!(output.status.code(), Some(2), "{}", printed(&output));
    let reason =
        "`git rev-parse` failed (exit status: 128): fatal: not a git repository: /missing/.git/worktrees/design";
    assert!(logged(&output, "ERROR", reason), "{}", printed(&output));
}

/// A repository with no commit holds no file, which `git rev-parse` tells by its exit code 1 whatever it prints, so
/// each file stays plain PHP for git holding no commit of it, with `GIT_TRACE` on.
#[test]
fn a_repository_with_no_commit_holds_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    project(root, &[("Clean.php", CLASS)]);
    let init = Command::new("git").args(["init", "--quiet"]).current_dir(root).output().unwrap();
    assert!(init.status.success());

    let output = convert(root, &[("GIT_TRACE", "1")]);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
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

/// Shape decides before git, so a script stays plain PHP for being a script, with no `git` to run.
#[test]
fn a_script_stays_plain_php_without_git() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    project(root, &[("helpers.php", SCRIPT)]);

    let output = convert(root, &[("PATH", "")]);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    let reason = "  src/helpers.php: it declares no class, interface or enum, so it is a script";
    assert!(logged(&output, "WARN", reason), "{}", printed(&output));
}
