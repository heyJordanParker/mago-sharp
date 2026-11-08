#![allow(clippy::expect_used, clippy::missing_panics_doc)]

//! Runs `mago analyze` on named paths of a project whose extension judges files against each other.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use mago_database::process;
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
    process::output(&mut command(workspace, arguments)).expect("mago runs")
}

fn analyze_stdin(workspace: &Path, arguments: &[&str], contents: &str) -> Output {
    let mut child = process::spawn(
        command(workspace, arguments).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()),
    )
    .expect("mago runs");
    child.stdin.take().expect("stdin").write_all(contents.as_bytes()).expect("stdin accepts the buffer");
    child.wait_with_output().expect("mago finishes")
}

/// The first report a `--watch` run prints, taken before the run is stopped.
fn first_watch_report(workspace: &Path, arguments: &[&str]) -> Vec<Value> {
    let mut child =
        process::spawn(command(workspace, arguments).stdout(Stdio::piped()).stderr(Stdio::null())).expect("mago runs");
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
        assert!(
            process::status(Command::new("git").args(arguments).current_dir(directory.path())).expect("git").success()
        );
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
        reports_the_duplicate_route_in(&issues, "src/Second.php"),
        "the mutated file is checked against the rest of the project: {issues:#?}"
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue["code"] == "invalid-return-statement" && primary_file(issue) == Some("src/Second.php")),
        "the mutated file's own issue is reported at the original's path: {issues:#?}"
    );
    assert!(
        issues.iter().all(|issue| primary_file(issue) == Some("src/Second.php")),
        "only the mutated file is reported: {issues:#?}"
    );
}

/// The plain PHP class `App\Box` of a project whose `composer.json` maps `App` to `src/`, and `drafts/`, which the
/// project's `mago.toml` excludes.
fn namespaced_project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::create_dir_all(directory.path().join("src")).expect("src directory");
    std::fs::create_dir_all(directory.path().join("drafts")).expect("drafts directory");
    std::fs::write(
        directory.path().join("composer.json"),
        "{\n    \"name\": \"acme/shop\",\n    \"type\": \"project\",\n    \"autoload\": {\n        \"psr-4\": {\n            \"App\\\\\": \"src/\"\n        }\n    }\n}\n",
    )
    .expect("composer.json");
    std::fs::write(
        directory.path().join("mago.toml"),
        "php-version = \"8.4\"\n\n[source]\npaths = [\"src\", \"drafts\"]\nexcludes = [\"drafts\"]\n",
    )
    .expect("mago.toml");
    std::fs::write(directory.path().join("src/Box.php"), "<?php\n\nnamespace App;\n\nfinal class Box\n{\n}\n")
        .expect("Box.php");

    directory
}

/// A PHP# draft of `Box` that declares a namespace its folder doesn't hold.
const MISPLACED_BOX: &str = "namespace Other;\n\npublic final class Box\n{\n}\n";

/// Each issue of `issues` as `file code: message`.
fn located(issues: &[Value]) -> Vec<String> {
    let mut located: Vec<String> = issues
        .iter()
        .map(|issue| {
            format!(
                "{} {}: {}",
                primary_file(issue).unwrap_or_default(),
                issue["code"].as_str().unwrap_or_default(),
                issue["message"].as_str().unwrap_or_default()
            )
        })
        .collect();
    located.sort();

    located
}

/// The issues the draft `MISPLACED_BOX` gets where it stands in place of `src/Box.php`, as `src/Box.sharp`.
fn in_place_issues() -> Vec<String> {
    let project = namespaced_project();
    std::fs::remove_file(project.path().join("src/Box.php")).expect("the original moves aside");
    std::fs::write(project.path().join("src/Box.sharp"), MISPLACED_BOX).expect("the draft in place");

    located(&issues(&analyze(project.path(), &[])))
}

/// A draft is checked as the file it replaces would be at the original's path, whatever folder holds it: a draft
/// outside the project takes the original's package and folder namespace, so it gets the folder-namespace error it
/// gets in place, named at the original's path with the draft's own extension.
#[test]
fn a_draft_outside_the_project_is_checked_in_place_of_the_file_it_replaces() {
    let in_place = in_place_issues();
    assert!(
        in_place.contains(&"src/Box.sharp semantics: This file's folder holds the namespace `App`, so the file declares `namespace App;`.".to_owned()),
        "{in_place:#?}"
    );

    let project = namespaced_project();
    let outside = tempfile::tempdir().expect("a folder outside the project");
    let draft = outside.path().join("Box.sharp");
    std::fs::write(&draft, MISPLACED_BOX).expect("the draft");
    let substitution = format!("{}={}", project.path().join("src/Box.php").display(), draft.display());

    assert_eq!(located(&issues(&analyze(project.path(), &["--substitute", &substitution]))), in_place);
}

/// A draft's own folder never excludes it: one in a folder the project excludes is checked as the original would be.
#[test]
fn a_draft_in_an_excluded_folder_is_checked_in_place_of_the_file_it_replaces() {
    let in_place = in_place_issues();

    let project = namespaced_project();
    let draft = project.path().join("drafts/Box.sharp");
    std::fs::write(&draft, MISPLACED_BOX).expect("the draft");
    let substitution = format!("{}={}", project.path().join("src/Box.php").display(), draft.display());

    assert_eq!(located(&issues(&analyze(project.path(), &["--substitute", &substitution]))), in_place);
}
