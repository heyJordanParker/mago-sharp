//! A process Mago starts in a process group of its own, so Mago can end it with every process it started.
//!
//! Such a process is the `docker compose` process a `docker compose exec` wrapper starts on the host, or each `lean` a
//! `lake` starts. The group
//! is not Mago's own, so a terminal's Ctrl-C reaches it only through `forward`, which ends every such group on each
//! signal that ends Mago, as the signal ended them while they ran in Mago's group. Groups exist on macOS and Linux, the
//! platforms PHP# runs on.

use std::io;
use std::process::Child;
use std::process::ChildStdin;
use std::process::Command;
#[cfg(unix)]
use std::sync::Once;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
#[cfg(unix)]
use std::sync::atomic::AtomicPtr;
#[cfg(unix)]
use std::sync::atomic::Ordering;
#[cfg(target_os = "macos")]
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use mago_database::process;

/// The first and the longest pause before `ProcessGroup::end` signals a group again whose processes still run, as they
/// do until the kernel has ended them.
#[cfg(target_os = "macos")]
const INITIAL_END_POLL_INTERVAL: Duration = Duration::from_micros(50);
#[cfg(target_os = "macos")]
const MAXIMUM_END_POLL_INTERVAL: Duration = Duration::from_millis(1);
/// The longest `ProcessGroup::end` signals a group again whose processes still run. A process that SIGKILL cannot end,
/// as one the kernel holds on a stalled disk, keeps it signalling past this, and it fails with an error that names the
/// group. On the CI Mac beside the workspace's tests, 13,500 ends of a shell and two `sleep`s took at most 753 ms, and
/// 900 ends of a process holding 1 GiB, as a Lean process does, at most 44 ms.
#[cfg(target_os = "macos")]
const END_TIMEOUT: Duration = Duration::from_secs(10);

/// A child process that leads a process group of its own.
///
/// Dropping it ends the group and reaps the leader, unless Mago already waited for the leader, as tokio's
/// `kill_on_drop` does, so a group Mago stops tracking, as a panic stops it, leaves nothing running.
#[derive(Debug)]
pub struct ProcessGroup {
    leader: Child,
    /// The slot that holds the group's id for `forward`.
    #[cfg(unix)]
    slot: &'static Slot,
    /// Why a process of the group outlived an end, which every later end returns at once rather than wait again.
    #[cfg(target_os = "macos")]
    outlived: Option<String>,
}

impl ProcessGroup {
    /// Starts `command` in a process group of its own, which a signal that ends Mago ends too.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be started, or if a signal is ending Mago.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            std::os::unix::process::CommandExt::process_group(command, 0);
            forward_signals();
            let slot = Slot::take();
            // A signal that arrived before the slot held `STARTING` waited for no group, so it is ending Mago. Starting
            // a process now would only leave one behind.
            let signal = PENDING.load(Ordering::SeqCst);
            if signal != 0 {
                slot.group.store(0, Ordering::SeqCst);
                forward(signal);
                return Err(io::Error::other("a signal is ending Mago"));
            }
            let spawned = process::spawn(command);
            // `forward` writes `PENDING` before it reads the slots, and this thread writes its slot before it reads
            // `PENDING`, so at least one of them ends the group for a signal that arrived during the spawn.
            let group = spawned.as_ref().ok().and_then(|child| libc::pid_t::try_from(child.id()).ok()).unwrap_or(0);
            slot.group.store(group, Ordering::SeqCst);
            let signal = PENDING.load(Ordering::SeqCst);
            if signal != 0 {
                forward(signal);
            }

            Ok(Self {
                leader: spawned?,
                slot,
                #[cfg(target_os = "macos")]
                outlived: None,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self { leader: process::spawn(command)? })
        }
    }

    /// The process that leads the group, the one `spawn` started.
    pub fn leader(&mut self) -> &mut Child {
        &mut self.leader
    }

    /// Whether the leader has exited, which, unlike `Child::try_wait`, leaves it to be waited for: its id names the
    /// group until then, so `end` stays safe after it.
    #[must_use]
    pub fn leader_exited(&self) -> bool {
        !matches!(self.leader_state(), Ok(false))
    }

    /// Ends the leader and every process of its group with `SIGKILL`, such as the worker a wrapper forked. The leader's
    /// id names the group until Mago waits for the leader, so Mago ends the group before it waits. Elsewhere than macOS
    /// and Linux, it kills the leader alone.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be sent, as when every process of the group has ended, and on macOS one of
    /// kind `TimedOut` that names the group when a process of it still runs `END_TIMEOUT` after the first signal. Once
    /// an end timed out, each later one that still finds a process of the group returns that error at once.
    pub fn end(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let group = libc::pid_t::try_from(self.leader.id()).map_err(io::Error::other)?;
            // SAFETY: `killpg` only sends a signal, to the group the leader leads since it started with `process_group(0)`.
            // The leader is not waited for yet, so its id still names the group.
            let signal = || unsafe { libc::killpg(group, libc::SIGKILL) } == 0;
            #[cfg(test)]
            let signal = || signal() || unending(group);
            if !signal() {
                return Err(io::Error::last_os_error());
            }
            // macOS gives a process that a process of the group starts while the signal goes out none of it, so it would
            // live on with what the group held, as a worker's pipes, and Mago would wait for them to close forever.
            // Linux hands such a process the signal too.
            #[cfg(target_os = "macos")]
            {
                if let Some(outlived) = &self.outlived {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, outlived.clone()));
                }
                signal_until_ended(group, END_TIMEOUT, signal)
                    .inspect_err(|error| self.outlived = Some(error.to_string()))?;
            }

            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.leader.kill()
        }
    }

    /// Whether the leader has exited, without waiting for it: an error once Mago waited for it.
    #[cfg(unix)]
    fn leader_state(&self) -> io::Result<bool> {
        // SAFETY: `siginfo_t` is plain data, which zeroes make valid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `waitid` only fills `info`, and `WNOWAIT` leaves the leader to be waited for.
        if unsafe {
            libc::waitid(libc::P_PID, self.leader.id(), &raw mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        } != 0
        {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `waitid` filled `info`, whose process id stays 0 while the leader runs.
        Ok(unsafe { info.si_pid() } != 0)
    }

    #[cfg(not(unix))]
    fn leader_state(&self) -> io::Result<bool> {
        Ok(true)
    }
}

/// Sends SIGKILL to the group `group` again while `signal`, which sends it, says a process of the group still ran, for
/// at most `timeout`. `killpg` succeeds while a process of the group still runs, and fails once only exited ones are
/// left.
///
/// # Errors
///
/// Returns an error of kind `TimedOut` that names the group when a process of it still runs after `timeout`.
#[cfg(target_os = "macos")]
fn signal_until_ended(group: libc::pid_t, timeout: Duration, mut signal: impl FnMut() -> bool) -> io::Result<()> {
    let started = Instant::now();
    let mut poll_interval = INITIAL_END_POLL_INTERVAL;
    while signal() {
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("process group {group} still ran {} ms after SIGKILL", timeout.as_millis()),
            ));
        }
        std::thread::sleep(poll_interval);
        poll_interval = (poll_interval * 2).min(MAXIMUM_END_POLL_INTERVAL);
    }

    Ok(())
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    /// Ends the group and reaps the leader unless Mago already waited for it, then frees the group's slot, so a signal
    /// no longer ends the group: once the group ends, its id may name another. When a process of the group outlives the
    /// end, the leader is reaped only if it has exited, since waiting for it could wait forever.
    fn drop(&mut self) {
        if self.leader_state().is_ok() {
            let _result = match self.end() {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => self.leader.try_wait().map(drop),
                _ => self.leader.wait().map(drop),
            };
        }
        self.slot.group.store(0, Ordering::SeqCst);
    }
}

/// The groups a test says SIGKILL cannot end, as the kernel cannot end a process it holds on a stalled disk.
#[cfg(all(test, unix))]
pub(crate) static UNENDING: std::sync::Mutex<Vec<libc::pid_t>> = std::sync::Mutex::new(Vec::new());

/// Whether a test said SIGKILL cannot end `group`, which `end` then finds running.
#[cfg(all(test, unix))]
fn unending(group: libc::pid_t) -> bool {
    UNENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&group)
}

/// Makes the calling thread's writes to `stdin`, a child's standard input, fail with `ErrorKind::BrokenPipe` once the
/// child has ended, rather than raise SIGPIPE.
///
/// Mago takes SIGPIPE's default action, which ends it, so `mago … | head` ends quietly, and a write to a child that
/// ended would end Mago with it. Each thread that writes a child's stdin calls this before its first write. Linux raises
/// SIGPIPE on the thread that wrote, so there this blocks SIGPIPE on the calling thread. macOS raises it on the whole
/// process unless the pipe says otherwise, so there this marks the pipe.
pub fn block_sigpipe(stdin: &ChildStdin) {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;

        /// `F_SETNOSIGPIPE`, which `libc` does not define: `#define F_SETNOSIGPIPE 73` in the macOS SDK's
        /// `usr/include/sys/fcntl.h`.
        const F_SETNOSIGPIPE: libc::c_int = 73;
        // SAFETY: `fcntl` only sets a flag of the pipe `stdin` holds open.
        unsafe { libc::fcntl(stdin.as_raw_fd(), F_SETNOSIGPIPE, 1) };
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = stdin;
        // SAFETY: `sigset_t` is plain data, which zeroes make valid.
        let mut signals: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `sigemptyset` only writes the set it is given.
        unsafe { libc::sigemptyset(&raw mut signals) };
        // SAFETY: `sigaddset` only writes the set it is given.
        unsafe { libc::sigaddset(&raw mut signals, libc::SIGPIPE) };
        // SAFETY: `pthread_sigmask` only adds `signals` to the calling thread's mask.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const signals, std::ptr::null_mut()) };
    }
    #[cfg(not(unix))]
    let _ = stdin;
}

/// What a slot holds while its group's leader starts, before its id is known.
#[cfg(unix)]
const STARTING: libc::pid_t = -1;

/// One process group `forward` ends: its id, `STARTING`, or 0 while no group holds the slot.
#[cfg(unix)]
#[derive(Debug)]
struct Slot {
    group: AtomicI32,
    /// The slot added before this one.
    next: Option<&'static Slot>,
}

/// The slot added last. A slot is never freed, only reused, so `forward` can walk the slots whenever a signal arrives,
/// and there are as many as the most groups Mago ran at once.
#[cfg(unix)]
static SLOTS: AtomicPtr<Slot> = AtomicPtr::new(std::ptr::null_mut());

/// The signal `forward` received, which `ProcessGroup::spawn` passes back to it once the group it started is known: a
/// signal can reach any of Mago's threads while one of them spawns.
#[cfg(unix)]
static PENDING: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
impl Slot {
    /// The slot added last, from which `next` reaches every other.
    fn last() -> Option<&'static Self> {
        // SAFETY: `SLOTS` holds null or a slot `take` leaked, which is never freed.
        unsafe { SLOTS.load(Ordering::SeqCst).as_ref() }
    }

    /// A free slot, which then holds `STARTING`, or a new one when every slot holds a group.
    fn take() -> &'static Self {
        let mut slot = Self::last();
        while let Some(free) = slot {
            if free.group.compare_exchange(0, STARTING, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                return free;
            }
            slot = free.next;
        }

        let added = Box::into_raw(Box::new(Self { group: AtomicI32::new(STARTING), next: None }));
        let mut last = SLOTS.load(Ordering::SeqCst);
        loop {
            // SAFETY: `last` is null or a slot this function leaked, which is never freed.
            let next = unsafe { last.as_ref() };
            // SAFETY: no other thread reads `added` before the exchange below publishes it.
            unsafe { (*added).next = next };
            match SLOTS.compare_exchange(last, added, Ordering::SeqCst, Ordering::SeqCst) {
                // SAFETY: `added` came from `Box::into_raw`, and a slot is never freed.
                Ok(_) => return unsafe { &*added },
                Err(actual) => last = actual,
            }
        }
    }
}

/// On each signal that ends Mago from a terminal or `kill`, which Mago does not ignore, ends every group a slot holds,
/// then ends Mago with it as before.
#[cfg(unix)]
fn forward_signals() {
    static FORWARDING: Once = Once::new();
    FORWARDING.call_once(|| {
        for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
            // SAFETY: `sigaction` is plain data, which zeroes make valid.
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: `sigaction` only reads the handler of `signal` into `previous`.
            if unsafe { libc::sigaction(signal, std::ptr::null(), &raw mut previous) } != 0
                || previous.sa_sigaction != libc::SIG_DFL
            {
                continue;
            }
            // SAFETY: as for `previous`.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            // `sigaction` takes the handler as its address.
            #[allow(clippy::fn_to_numeric_cast_any)]
            let handler = forward as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_sigaction = handler;
            action.sa_flags = libc::SA_RESETHAND;
            // SAFETY: `action` is a valid `sigaction`, and `forward` is async-signal-safe.
            unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) };
        }
    });
}

/// Ends every group a slot holds with `SIGKILL`, as `ProcessGroup::end` does, so no process of them dumps a core on
/// `SIGQUIT`, then raises `signal` again, which `SA_RESETHAND` made end Mago. While a group starts, it records `signal`
/// in `PENDING` and leaves the raise to the spawning thread, which sends `signal` on once the group is known.
#[cfg(unix)]
extern "C" fn forward(signal: libc::c_int) {
    PENDING.store(signal, Ordering::SeqCst);
    let mut starting = false;
    let mut slot = Slot::last();
    while let Some(held) = slot {
        match held.group.load(Ordering::SeqCst) {
            0 => {}
            STARTING => starting = true,
            group => {
                // SAFETY: `kill` is async-signal-safe.
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
        }
        slot = held.next;
    }
    if !starting {
        // SAFETY: `raise` is async-signal-safe.
        unsafe { libc::raise(signal) };
    }
}

#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// A process that SIGKILL cannot end keeps `signal` saying a process of the group runs, and the end stops at its
    /// timeout with the group's id.
    #[test]
    fn a_group_whose_process_never_ends_stops_its_end_at_the_timeout_and_names_the_group() {
        const TIMEOUT: Duration = Duration::from_millis(20);
        let started = Instant::now();

        let error = signal_until_ended(4321, TIMEOUT, || true).expect_err("the end should stop at its timeout");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "process group 4321 still ran 20 ms after SIGKILL");
        assert!(started.elapsed() >= TIMEOUT, "the end stopped after {:?}", started.elapsed());
    }

    /// A group whose end timed out answers each later end at once with the same error, and dropping it reaps a leader
    /// that has exited, so the leader is no zombie for as long as Mago runs.
    #[test]
    fn a_group_that_outlived_its_end_ends_again_at_once_and_its_drop_reaps_an_exited_leader() {
        let mut group = ProcessGroup::spawn(&mut Command::new("true")).expect("true should start");
        while !group.leader_exited() {
            std::thread::sleep(Duration::from_millis(1));
        }
        let id = libc::pid_t::try_from(group.leader().id()).expect("a process id fits a pid_t");
        UNENDING.lock().expect("no test panics with the list held").push(id);

        let first = group.end().expect_err("the end should time out");
        let started = Instant::now();
        let again = group.end().expect_err("the end should time out again");
        let ended_again_after = started.elapsed();
        drop(group);
        UNENDING.lock().expect("no test panics with the list held").retain(|unending| *unending != id);

        assert_eq!(first.kind(), io::ErrorKind::TimedOut);
        assert_eq!((again.kind(), again.to_string()), (first.kind(), first.to_string()));
        assert!(ended_again_after < END_TIMEOUT, "the second end took {ended_again_after:?}");
        // SAFETY: `siginfo_t` is plain data, which zeroes make valid, and `waitid` only fills it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: as for `info`; `WNOWAIT` leaves a leader that is still there to be waited for.
        let waited = unsafe {
            libc::waitid(libc::P_PID, id.cast_unsigned(), &raw mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        };
        assert_eq!(
            (waited, io::Error::last_os_error().raw_os_error()),
            (-1, Some(libc::ECHILD)),
            "the dropped group's leader should be reaped"
        );
    }
}
