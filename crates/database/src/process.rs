//! Every process Mago starts, one at a time.
//!
//! `spawn`, `output` and `status` start a process as `Command`'s methods of the same names do, under one lock, and
//! every process Mago starts goes through them. macOS has no `pipe2`, so `Command::spawn` creates each pipe of a process
//! before it marks the pipe close-on-exec. A process another thread starts in between inherits the pipe and holds it
//! open for as long as it runs. A worker that closed its stdin then still takes Mago's writes, and the stdout of a
//! worker that died stays open, so its requests wait for their deadline rather than fail with what the worker wrote. A
//! pool starts its workers at once. On a loaded Mac, 30 of 300 processes that closed their stdin while another thread
//! kept starting processes still took a write, and none did under this lock. Go's `syscall.ForkLock` holds the same
//! steps apart. This crate is the lowest one every crate that starts a process depends on.
//!
//! A program Mago just wrote, as the Lean it unpacks into its cache folder, can fail to start with `ETXTBSY` ("Text file
//! busy") on Linux. A process another thread started while the program was open for writing holds a copy of that
//! descriptor until its `exec` closes it, and the thread that started it can go on before that close, so the lock below
//! doesn't cover the window. A start that fails with it is retried after a pause that doubles each time, from 1 ms, for
//! 10 tries and 511 ms of pauses in all, far past the moment a starting process holds a descriptor.
//!
//! Every process runs with `SHARP_COMPILING=1`, as the PHP# engine runs its `sharp.compile_command`. A PHP# `php` that
//! has it set never runs that command, so an extension host's worker that loads a file the checker refused, under a
//! `php.ini` that names the command, gets the refusal instead of starting a compile, whose hosts would start another.

use std::io;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use crate::clock::Clock;
use crate::clock::SystemClock;

/// Held while a process starts, so no process starts while another one's pipes are created.
static SPAWNING: Mutex<()> = Mutex::new(());

/// The variable every process Mago starts runs with, which the PHP# engine reads.
const SHARP_COMPILING: &str = "SHARP_COMPILING";

/// The tries a start that fails with `ETXTBSY` gets, the first one included, each after a pause twice the last.
const BUSY_TRIES: u32 = 10;

/// Starts `command`, as `Command::spawn` does, with `SHARP_COMPILING=1`, and again while its program is busy.
///
/// # Errors
///
/// Returns the error `Command::spawn` returns, and for a program still busy after the last try, that error with the
/// tries it took.
#[allow(clippy::disallowed_methods, reason = "the one start every other one goes through, under SPAWNING")]
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    command.env(SHARP_COMPILING, "1");
    let mut pause = Duration::from_millis(1);
    let mut tries = 1;
    loop {
        let started = {
            let _spawning = SPAWNING.lock().unwrap_or_else(PoisonError::into_inner);
            command.spawn()
        };
        match started {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy && tries < BUSY_TRIES => {
                SystemClock.sleep(pause);
                pause *= 2;
                tries += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                return Err(io::Error::new(error.kind(), format!("{error}, on each of {BUSY_TRIES} tries")));
            }
            started => return started,
        }
    }
}

/// Runs `command` to its end and returns what it wrote, as `Command::output` does.
///
/// It always captures stdout and stderr and empties stdin, even where `command` set them otherwise, which
/// `Command::output` would keep.
///
/// # Errors
///
/// Returns the error starting or waiting for the process returns.
pub fn output(command: &mut Command) -> io::Result<Output> {
    spawn(command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()))?.wait_with_output()
}

/// Runs `command` to its end and returns how it exited, as `Command::status` does.
///
/// # Errors
///
/// Returns the error starting or waiting for the process returns.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    spawn(command)?.wait()
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used)]
mod tests {
    use std::process::Command;

    use super::output;

    /// A process Mago starts runs with `SHARP_COMPILING=1`, even where its command removed the variable.
    #[test]
    fn every_process_runs_with_sharp_compiling_set() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf %s \"$SHARP_COMPILING\""]).env_remove("SHARP_COMPILING");
        let printed = output(&mut command).expect("sh should run");

        assert_eq!(String::from_utf8_lossy(&printed.stdout), "1");
    }

    /// On Linux a program open for writing is busy, as one is while a process another thread started still holds the
    /// descriptor Mago wrote it through. Its start is tried again, and once every try found it busy the error names the
    /// tries. Closed, it starts.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_start_of_a_busy_program_is_tried_again_and_its_error_names_the_tries() {
        use std::os::unix::fs::PermissionsExt;

        let folder = tempfile::tempdir().expect("a temporary folder");
        let program = folder.path().join("program");
        std::fs::write(&program, "#!/bin/sh\nprintf started\n").expect("the program is written");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("the program runs");
        let writing = std::fs::OpenOptions::new().append(true).open(&program).expect("the program opens for writing");

        let busy = output(&mut Command::new(&program)).expect_err("a program open for writing is busy");
        drop(writing);
        let started = output(&mut Command::new(&program)).expect("a closed program starts");

        assert_eq!(busy.kind(), std::io::ErrorKind::ExecutableFileBusy);
        assert!(busy.to_string().ends_with(", on each of 10 tries"), "{busy}");
        assert_eq!(String::from_utf8_lossy(&started.stdout), "started");
    }
}
