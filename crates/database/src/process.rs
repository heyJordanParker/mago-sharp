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

use std::io;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::PoisonError;

/// Held while a process starts, so no process starts while another one's pipes are created.
static SPAWNING: Mutex<()> = Mutex::new(());

/// Starts `command`, as `Command::spawn` does.
///
/// # Errors
///
/// Returns the error `Command::spawn` returns.
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    let _spawning = SPAWNING.lock().unwrap_or_else(PoisonError::into_inner);

    command.spawn()
}

/// Runs `command` to its end and returns what it wrote, as `Command::output` does: its stdin is empty, and its stdout
/// and stderr are captured.
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
