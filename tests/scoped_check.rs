#![allow(clippy::expect_used, clippy::missing_panics_doc)]

//! Runs `mago analyze` on named paths of a project whose extension judges files against each other.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

mod common;

const FIRST: &str = "<?php\n\n// route: /home\nfunction first(): int { return 'text'; }\n";

const SECOND: &str = "<?php\n\n// route: /home\nfunction second(): int { return 2; }\n";

const SOURCE_PATHS: &str = "[source]\npaths = [\"src\"]\n\n";

fn workspace(repository: &Path, source: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let worker = repository.join("composer/tests/Sdk/Fixtures/analyzer-server-worker.php");
    std::fs::create_dir_all(directory.path().join("src")).expect("src directory");
    std::fs::write(
        directory.path().join("mago.toml"),
        format!(
            "php-version = \"8.4\"\n\n{source}[extension-hosts.proof]\ncommand = [\"php\", {:?}]\nworkers = 1\n",
            worker.display().to_string()
        ),
    )
    .expect("mago.toml");
    std::fs::write(directory.path().join("src/First.php"), FIRST).expect("first file");
    std::fs::write(directory.path().join("src/Second.php"), SECOND).expect("second file");
    directory
}

fn command(workspace: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
    command
        .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "json"])
        .args(arguments)
        .current_dir(workspace);
    command
}

fn analyze(workspace: &Path, arguments: &[&str]) -> Output {
    command(workspace, arguments).output().expect("mago runs")
}

fn analyze_stdin(workspace: &Path, arguments: &[&str], contents: &str) -> Output {
    let mut child = command(workspace, arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("mago runs");
    child.stdin.take().expect("stdin").write_all(contents.as_bytes()).expect("stdin accepts the buffer");
    child.wait_with_output().expect("mago finishes")
}

/// The first report a `--watch` run prints, taken before the run is stopped.
fn first_watch_report(workspace: &Path, arguments: &[&str]) -> Vec<Value> {
    let mut child =
        command(workspace, arguments).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("mago runs");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let report = serde_json::Deserializer::from_reader(stdout).into_iter::<Value>().next();
        let _ = sender.send(report);
    });

    let report = receiver.recv_timeout(Duration::from_secs(300));
    child.kill().expect("the watch run stops");
    child.wait().expect("the watch run exits");

    let report = report.expect("the watch run reports in time").expect("a report").expect("a JSON report");
    report["issues"].as_array().expect("an issue list").clone()
}

fn issues(output: &Output) -> Vec<Value> {
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("{error}: {}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    });
    report["issues"].as_array().expect("an issue list").clone()
}

fn primary_file(issue: &Value) -> Option<&str> {
    issue["annotations"]
        .as_array()?
        .iter()
        .find(|annotation| annotation["kind"] == "Primary")
        .and_then(|annotation| annotation["span"]["file_id"]["name"].as_str())
}

fn reports_the_duplicate_route_in(issues: &[Value], file: &str) -> bool {
    issues.iter().any(|issue| issue["code"] == "server-proof/duplicate-route" && primary_file(issue) == Some(file))
}

fn the_scoped_check_is_the_whole_check_filtered(source: &str) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scoped check test") {
        return;
    }

    let directory = workspace(repository, source);

    let whole = issues(&analyze(directory.path(), &[]));
    let scoped_output = analyze(directory.path(), &["src/Second.php"]);
    let scoped = issues(&scoped_output);

    assert!(
        reports_the_duplicate_route_in(&scoped, "src/Second.php"),
        "the cross-file rule sees the file the check did not name: {scoped:#?}"
    );
    assert!(
        whole.iter().any(|issue| primary_file(issue) == Some("src/First.php")),
        "the whole check reports in the unnamed file: {whole:#?}"
    );

    let mut filtered = whole
        .iter()
        .filter(|issue| primary_file(issue).is_none_or(|file| file == "src/Second.php"))
        .collect::<Vec<_>>();
    let mut scoped = scoped.iter().collect::<Vec<_>>();
    filtered.sort_by_key(|issue| issue.to_string());
    scoped.sort_by_key(|issue| issue.to_string());
    assert_eq!(scoped, filtered);
    assert!(!scoped_output.status.success(), "the duplicate route fails the check");
}

#[test]
fn a_scoped_check_reports_what_a_whole_check_reports_in_the_named_file() {
    the_scoped_check_is_the_whole_check_filtered(SOURCE_PATHS);
}

#[test]
fn a_project_with_no_source_paths_is_checked_whole_around_the_named_file() {
    the_scoped_check_is_the_whole_check_filtered("");
}

#[test]
fn a_scoped_check_runs_node_hooks_only_in_the_named_files() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scoped check test") {
        return;
    }

    let directory = workspace(repository, SOURCE_PATHS);
    std::fs::write(directory.path().join("src/Third.php"), "<?php\n\n// node hook: fail\nfunction third(): void {}\n")
        .expect("third file");

    let whole = analyze(directory.path(), &[]);
    assert!(
        String::from_utf8_lossy(&whole.stderr).contains("The node hook ran on `src/Third.php`."),
        "the node hook fails in the marked file: {}",
        String::from_utf8_lossy(&whole.stderr)
    );

    let scoped = issues(&analyze(directory.path(), &["src/Second.php"]));
    assert!(
        scoped
            .iter()
            .any(|issue| issue["code"] == "server-proof/node" && primary_file(issue) == Some("src/Second.php")),
        "the node hook runs in the named file: {scoped:#?}"
    );
    assert!(
        reports_the_duplicate_route_in(&scoped, "src/Second.php"),
        "the cross-file rule still sees the unnamed files: {scoped:#?}"
    );
}

#[test]
fn every_way_of_naming_a_file_checks_it_against_the_whole_project() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scoped check test") {
        return;
    }

    let directory = workspace(repository, SOURCE_PATHS);
    for arguments in [&["init", "--quiet"][..], &["add", "src/Second.php"][..]] {
        assert!(Command::new("git").args(arguments).current_dir(directory.path()).status().expect("git").success());
    }

    for (entry_point, issues) in [
        ("--staged", issues(&analyze(directory.path(), &["--staged"]))),
        ("--stdin-input", issues(&analyze_stdin(directory.path(), &["src/Second.php", "--stdin-input"], SECOND))),
        ("--watch", first_watch_report(directory.path(), &["--watch", "src/Second.php"])),
    ] {
        assert!(reports_the_duplicate_route_in(&issues, "src/Second.php"), "{entry_point}: {issues:#?}");
        assert!(
            issues.iter().all(|issue| primary_file(issue) != Some("src/First.php")),
            "{entry_point} reports only in the named file: {issues:#?}"
        );
    }
}

#[test]
fn a_substituted_file_is_checked_in_place_of_the_file_it_replaces() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scoped check test") {
        return;
    }

    let directory = workspace(repository, SOURCE_PATHS);
    let mutated = directory.path().join("tmp/Second.php");
    std::fs::create_dir_all(directory.path().join("tmp")).expect("tmp directory");
    std::fs::write(&mutated, SECOND.replace("return 2;", "return 'mutated';")).expect("mutated file");
    let original = directory.path().join("src/Second.php");
    let substitution = format!("{}={}", original.display(), mutated.display());

    let issues = issues(&analyze(directory.path(), &["--substitute", &substitution, "src/Second.php"]));

    assert!(
        reports_the_duplicate_route_in(&issues, "tmp/Second.php"),
        "the mutated file is checked against the rest of the project: {issues:#?}"
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue["code"] == "invalid-return-statement" && primary_file(issue) == Some("tmp/Second.php")),
        "the mutated file's own issue is reported: {issues:#?}"
    );
    assert!(
        issues.iter().all(|issue| primary_file(issue) == Some("tmp/Second.php")),
        "only the mutated file is reported: {issues:#?}"
    );
}
