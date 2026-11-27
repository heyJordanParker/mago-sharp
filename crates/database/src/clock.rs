//! Time, as PHP#'s `Clock` effect gives it to a program.
//!
//! Code that waits on a deadline reads the clock it is given, so a test of the deadline runs on a [`FakeClock`] it
//! moves by hand and asserts what happens before, at and after the deadline exactly, as `Sharp.Testing.FakeClock` and
//! tokio's paused clock do (#958). A wait for another process's event, which no clock can move, polls it with a
//! ceiling through [`poll_until`].

use std::time::Duration;
use std::time::Instant;

/// A clock that code waiting on a deadline reads and sleeps on.
pub trait Clock: Send + Sync {
    /// The instant it is now.
    fn now(&self) -> Instant;

    /// Waits `duration`.
    fn sleep(&self, duration: Duration);
}

/// The operating system's monotonic clock, and a sleep of the calling thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the system clock's sleep is the one real sleep, which every wait reaches"
    )]
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A clock that holds a set instant until [`FakeClock::advance`] moves it, and whose sleep moves it by the sleep, as
/// tokio's paused clock advances a sleep at once.
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug)]
pub struct FakeClock {
    started: Instant,
    moved: std::sync::Mutex<Duration>,
}

#[cfg(any(test, feature = "test-util"))]
impl FakeClock {
    /// A clock at the instant it is made.
    #[must_use]
    pub fn new() -> Self {
        Self { started: Instant::now(), moved: std::sync::Mutex::new(Duration::ZERO) }
    }

    /// Moves the clock `duration` forward.
    pub fn advance(&self, duration: Duration) {
        *self.moved.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += duration;
    }

    /// How far the clock has moved since it was made.
    #[must_use]
    pub fn moved(&self) -> Duration {
        *self.moved.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(any(test, feature = "test-util"))]
impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-util"))]
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.started + self.moved()
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

/// The first and the longest pause between two looks of [`poll_until`].
const FIRST_POLL: Duration = Duration::from_millis(1);
const LONGEST_POLL: Duration = Duration::from_millis(100);

/// Whether `condition` held within `ceiling` of the system clock.
///
/// It looks at once, then after pauses that double up to 100 ms. It waits for an event of another process, which no
/// clock can move, as a process ending or a file it writes, so the ceiling only bounds a failure and asserts nothing
/// about how long the event took.
pub fn poll_until(ceiling: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let clock = SystemClock;
    let started = clock.now();
    let mut pause = FIRST_POLL;
    loop {
        if condition() {
            return true;
        }
        if clock.now().duration_since(started) >= ceiling {
            return false;
        }
        clock.sleep(pause);
        pause = (pause * 2).min(LONGEST_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake clock moves only when it is advanced or slept on, by exactly that much.
    #[test]
    fn a_fake_clock_moves_by_each_advance_and_sleep_alone() {
        let clock = FakeClock::new();
        let started = clock.now();

        clock.advance(Duration::from_millis(3));
        clock.sleep(Duration::from_micros(50));

        assert_eq!(clock.now().duration_since(started), Duration::from_micros(3_050));
        assert_eq!(clock.moved(), Duration::from_micros(3_050));
    }

    /// A condition that holds from the start ends the poll at its first look, and one that never holds ends it at its
    /// ceiling with false.
    #[test]
    fn a_poll_ends_when_its_condition_holds_or_at_its_ceiling() {
        let mut looks = 0;
        assert!(poll_until(Duration::from_mins(1), || {
            looks += 1;
            true
        }));
        assert_eq!(looks, 1);

        assert!(!poll_until(Duration::ZERO, || false));
    }
}
