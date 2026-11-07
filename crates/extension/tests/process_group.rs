#![cfg(unix)]
#![allow(clippy::expect_used)]

//! Drops process groups whose leader keeps starting processes. The test starts processes as fast as `sh` can fork, so
//! it runs in a test binary of its own, where no other test's process starts beside it.

use std::io::BufRead;
use std::io::BufReader;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use mago_extension::ProcessGroup;

/// A group whose leader keeps starting a process and ending it, so a process is starting whenever the group ends.
/// On macOS, a process that starts while the group's `SIGKILL` goes out never gets it and lives on, holding all
/// the group held, as a worker's pipes. Dropping the group ends it too.
#[test]
fn dropping_a_group_ends_the_process_its_leader_was_starting() {
    for _ in 0..20 {
        let mut command = Command::new("sh");
        command.args(["-c", "echo started; while :; do sleep 2147483647 & kill $!; done"]).stdout(Stdio::piped());
        let mut group = ProcessGroup::spawn(&mut command).expect("the group should start");
        let id = libc::pid_t::try_from(group.leader().id()).expect("the leader's id should be a pid_t");
        let stdout = group.leader().stdout.take().expect("the leader should have a stdout");
        BufReader::new(stdout).read_line(&mut String::new()).expect("the leader should say it started");

        drop(group);

        assert!(ends_within_ten_seconds(id), "a process of group {id} still ran after the group was dropped");
    }
}

/// Whether every process of `group` ends within 10 s. One that still runs then is ended, so a failing test leaves
/// nothing running.
fn ends_within_ten_seconds(group: libc::pid_t) -> bool {
    const TIMEOUT: Duration = Duration::from_secs(10);

    let deadline = Instant::now() + TIMEOUT;
    // SAFETY: `killpg` with signal 0 only checks whether a process of `group` runs.
    while unsafe { libc::killpg(group, 0) } == 0 {
        if Instant::now() >= deadline {
            // SAFETY: `killpg` only sends a signal, to the group the test started.
            while unsafe { libc::killpg(group, libc::SIGKILL) } == 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    true
}
