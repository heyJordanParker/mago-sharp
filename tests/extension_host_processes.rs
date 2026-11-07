#![cfg(unix)]
#![allow(clippy::expect_used, clippy::missing_panics_doc, clippy::panic)]

//! Runs `mago analyze` on projects whose extension host's workers stop, and checks that `mago` ends them and itself.

use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use mago_database::process;

use crate::common;

/// The longest a test waits for `mago` to end: far past each deadline the tests set.
const OUTER_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest a test waits for a worker to end once `mago` ended: far past the milliseconds it takes, and far short
/// of the 600 s a stuck worker sleeps.
const ENDED_WITHIN: Duration = Duration::from_secs(10);

/// An extension host whose plugin `hung-function` has a hook on each function that never finishes, so its worker
/// stops reading its stdin. The hook asks for the source of each file it inspects, so each batch of files Mago sends
/// holds their source. It appends its process id to the file `$argv[2]` names.
const HOST_WHOSE_FUNCTION_HOOK_NEVER_FINISHES: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\FileAnalysisRequirement;
use Mago\Sdk\Analyzer\NodeAnalysisContext;
use Mago\Sdk\Analyzer\NodeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;

require $argv[1];
file_put_contents($argv[2], getmypid() . "\n", FILE_APPEND);

final class HungFunction implements NodeAnalysisHook
{
    public function getTargets(): array
    {
        return [NodeKind::Function];
    }

    public function getRequirements(): array
    {
        return [FileAnalysisRequirement::SourceText];
    }

    public function analyze(NodeAnalysisContext $context): void
    {
        while (true) {
            sleep(1);
        }
    }
}

final class HungPlugin implements Plugin
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('hung-function', 'Hung function', 'Never finishes a function.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerNodeAnalysisHook(new HungFunction());
    }

    public function getRequirements(): array
    {
        return [];
    }
}

(new Worker(new Extension('acme/function', 'Function', '1.0.0', analyzerPlugins: [new HungPlugin()])))->run();
"#;

/// A worker that stops reading its stdin, here in an after-file hook that never finishes, leaves Mago's write of the
/// next batch of files unfinished once the batch no longer fits in the pipe. `mago analyze` still fails at the host's
/// deadline with the host's name, rather than wait on the write, or die of the SIGPIPE the write raises once Mago ends
/// the worker.
#[test]
fn a_host_whose_worker_stops_reading_fails_mago_analyze_at_its_deadline() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the stuck worker test") {
        return;
    }

    let workspace = tempfile::tempdir().expect("temporary workspace");
    let pids = workspace.path().join("pids");
    std::fs::write(workspace.path().join("host.php"), HOST_WHOSE_FUNCTION_HOOK_NEVER_FINISHES).expect("host.php");
    std::fs::write(
        workspace.path().join("mago.toml"),
        format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n\n[extension-hosts.acme]\nworkers = 1\nrequest-timeout-ms = 3000\ncommand = [\"php\", \"host.php\", {:?}, {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string(),
            pids.display().to_string(),
        ),
    )
    .expect("mago.toml");
    // Three batches of 32 files, each batch far past the 64 KiB a pipe holds on macOS and Linux.
    std::fs::create_dir_all(workspace.path().join("src")).expect("src");
    let padding = format!("// {}\n", "x".repeat(4096));
    for index in 0..96 {
        std::fs::write(
            workspace.path().join(format!("src/File{index}.php")),
            format!("<?php\n\n{padding}function file{index}(): int {{ return {index}; }}\n"),
        )
        .expect("source file");
    }

    let analyze = process::spawn(mago(workspace.path()).stderr(Stdio::piped())).expect("mago runs");
    let (status, stderr) = finish(analyze);
    let survivors = end_survivors(&pids);

    assert_eq!(status.code(), Some(2), "mago ended with {status}, and wrote: {stderr}");
    // The request that timed out says so, and each other request of the worker fails with that failure. Which of them
    // stops the analysis first varies, so either error may be the one `mago` prints.
    assert!(
        stderr.contains("extension host `acme`")
            && stderr.contains("made no progress for 3000 ms on the hooks of src/"),
        "mago wrote no timeout: {stderr}"
    );
    assert_eq!(survivors, Vec::<String>::new(), "workers outlived mago");
}

/// The terminal's Ctrl-C sends SIGINT to `mago`'s process group, which no longer holds its workers, so `mago` ends the
/// worker's own group before it ends.
#[test]
fn ctrl_c_ends_the_workers_of_an_extension_host() {
    let (workspace, mut analyze, workers) = stuck_workers(1);

    // SAFETY: `kill` only sends a signal, to the group `mago` leads, as a terminal does.
    assert_eq!(unsafe { libc::kill(-group_of(&analyze), libc::SIGINT) }, 0);

    assert_eq!(wait(&mut analyze).signal(), Some(libc::SIGINT));
    assert_eq!(end_survivors(&workspace.path().join("pids")), Vec::<String>::new(), "of workers {workers:?}");
}

/// One signal that ends `mago` ends every worker group it runs, here three, though it reaches `mago` alone, as `kill`
/// sends it.
#[test]
fn one_signal_to_mago_ends_every_group_of_its_workers() {
    let (workspace, mut analyze, workers) = stuck_workers(3);

    // SAFETY: `kill` only sends a signal, to `mago` alone.
    assert_eq!(unsafe { libc::kill(group_of(&analyze), libc::SIGTERM) }, 0);

    assert_eq!(wait(&mut analyze).signal(), Some(libc::SIGTERM));
    assert_eq!(end_survivors(&workspace.path().join("pids")), Vec::<String>::new(), "of workers {workers:?}");
}

/// A project whose extension host has `count` workers that never answer, and `mago analyze` on it, in a process group
/// of its own as a terminal starts it, once every worker has started. Returns the workspace, `mago`, and the workers'
/// process ids.
fn stuck_workers(count: usize) -> (tempfile::TempDir, Child, Vec<String>) {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let pids = workspace.path().join("pids");
    std::fs::create_dir_all(workspace.path().join("src")).expect("src");
    std::fs::write(workspace.path().join("src/User.php"), "<?php\n\nfinal class User {}\n").expect("source file");
    std::fs::write(
        workspace.path().join("mago.toml"),
        format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n\n[extension-hosts.stuck]\nworkers = {count}\nrequest-timeout-ms = 60000\ncommand = [\"sh\", \"-c\", \"echo $$ >> '{}'; exec sleep 600\"]\n",
            pids.display(),
        ),
    )
    .expect("mago.toml");

    let analyze = process::spawn(mago(workspace.path()).stderr(Stdio::null()).process_group(0)).expect("mago runs");
    let started = Instant::now();
    let workers = loop {
        let workers = process_ids(&pids);
        if workers.len() == count {
            break workers;
        }
        assert!(started.elapsed() < OUTER_TIMEOUT, "{} of {count} workers started", workers.len());
        std::thread::sleep(Duration::from_millis(10));
    };

    (workspace, analyze, workers)
}

/// `mago analyze` in `workspace`.
fn mago(workspace: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
    command
        .args(["--no-version-check", "--colors", "never", "--threads", "4", "analyze"])
        .current_dir(workspace)
        .stdout(Stdio::null());
    command
}

/// The process group `mago` leads, which is its own id.
fn group_of(analyze: &Child) -> libc::pid_t {
    libc::pid_t::try_from(analyze.id()).expect("the process id fits a pid_t")
}

/// How `analyze` ended, and what it wrote to stderr.
fn finish(mut analyze: Child) -> (ExitStatus, String) {
    let mut stderr = analyze.stderr.take().expect("stderr is piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut stderr, &mut text).expect("stderr reads");
        text
    });
    let status = wait(&mut analyze);

    (status, reader.join().expect("the stderr reader joins"))
}

/// Waits for `analyze` to end, and kills it after `OUTER_TIMEOUT`, so a `mago` that hangs fails the test rather than
/// hang the test run.
fn wait(analyze: &mut Child) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = analyze.try_wait().expect("mago is waited for") {
            return status;
        }
        if started.elapsed() > OUTER_TIMEOUT {
            let _result = analyze.kill();
            panic!("mago still ran after {OUTER_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The ids of the workers that still run after `ENDED_WITHIN` of the ones `pids` lists, each of which this then kills,
/// so a failed test leaves no worker behind.
fn end_survivors(pids: &Path) -> Vec<String> {
    let started = Instant::now();
    let mut running = process_ids(pids);
    while !running.is_empty() && started.elapsed() < ENDED_WITHIN {
        running.retain(|pid| runs(pid));
        std::thread::sleep(Duration::from_millis(10));
    }
    for pid in &running {
        let _result = process::status(Command::new("kill").args(["-KILL", pid]));
    }

    running
}

/// The process ids the file `pids` lists, one per line.
fn process_ids(pids: &Path) -> Vec<String> {
    std::fs::read_to_string(pids).unwrap_or_default().lines().map(str::to_owned).collect()
}

/// Whether process `pid` runs. One that ended and that nothing reaped yet counts as ended.
fn runs(pid: &str) -> bool {
    let state = process::output(Command::new("ps").args(["-o", "stat=", "-p", pid])).expect("ps runs").stdout;
    let state = state.trim_ascii();
    !state.is_empty() && !state.starts_with(b"Z")
}
