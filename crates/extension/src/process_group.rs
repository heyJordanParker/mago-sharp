//! A process Mago starts in a process group of its own, so Mago can end it with every process it started.
//!
//! Such a process is the worker a wrapper like `docker compose exec` forks, or each `lean` a `lake` starts. The group
//! is not Mago's own, so a terminal's Ctrl-C reaches it only through `forward`, which ends every such group on each
//! signal that ends Mago, as the signal ended them while they ran in Mago's group.

use std::io;
use std::process::Child;
use std::process::Command;
#[cfg(unix)]
use std::sync::Once;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
#[cfg(unix)]
use std::sync::atomic::AtomicPtr;
#[cfg(unix)]
use std::sync::atomic::Ordering;

/// A child process that leads a process group of its own. Windows has no process groups, so there it is the child
/// alone.
#[derive(Debug)]
pub struct ProcessGroup {
    leader: Child,
    /// The slot that holds the group's id for `forward`.
    #[cfg(unix)]
    slot: &'static Slot,
}

impl ProcessGroup {
    /// Starts `command` in a process group of its own, which a signal that ends Mago ends too.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be started.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            std::os::unix::process::CommandExt::process_group(command, 0);
            forward_signals();
            let slot = Slot::take();
            let spawned = command.spawn();
            // `forward` writes `PENDING` before it reads the slots, and this thread writes its slot before it reads
            // `PENDING`, so at least one of them ends the group for a signal that arrived during the spawn.
            let group = spawned.as_ref().ok().and_then(|child| libc::pid_t::try_from(child.id()).ok()).unwrap_or(0);
            slot.group.store(group, Ordering::SeqCst);
            let signal = PENDING.load(Ordering::SeqCst);
            if signal != 0 {
                forward(signal);
            }

            Ok(Self { leader: spawned?, slot })
        }
        #[cfg(not(unix))]
        {
            Ok(Self { leader: command.spawn()? })
        }
    }

    /// The process that leads the group, the one `spawn` started.
    pub fn leader(&mut self) -> &mut Child {
        &mut self.leader
    }

    /// Ends the leader and every process of its group with `SIGKILL`, such as the worker a wrapper forked. The leader's
    /// id names the group until Mago waits for the leader, so Mago ends the group before it waits. Windows has no
    /// groups, so there it kills the leader alone.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal cannot be sent, as when every process of the group has ended.
    pub fn end(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let group = libc::pid_t::try_from(self.leader.id()).map_err(io::Error::other)?;
            // SAFETY: `killpg` only sends a signal, to the group the leader leads since it started with `process_group(0)`.
            if unsafe { libc::killpg(group, libc::SIGKILL) } != 0 {
                return Err(io::Error::last_os_error());
            }

            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.leader.kill()
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    /// Frees the group's slot, so a signal no longer ends the group: once the group ends, its id may name another.
    fn drop(&mut self) {
        self.slot.group.store(0, Ordering::SeqCst);
    }
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
