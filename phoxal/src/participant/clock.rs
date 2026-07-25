//! The clock + time source (D34).
//!
//! No participant mints its own clock; the runner owns one [`ClockSource`] and stamps
//! every `StepContext`/`produced_at_ns` from it, so all participants share one
//! logical-time domain. Three sources are envisaged: **real** (host-monotonic),
//! **simulation** (subscribe the authoritative `simulation/clock`), and **test**
//! (an injectable fake). The first slice ships real + test; the simulation source
//! lands with the Webots port.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use crate::bus::LogicalTime;

/// A source of logical robot time.
pub trait ClockSource: Send + Sync + 'static {
    /// The current logical time. Within an epoch this strictly increases.
    fn now(&self) -> LogicalTime;
}

/// Host-wide real clock (D34, "host-monotonic domain shared across processes").
///
/// `produced_at_ns` is stamped from this clock and is compared *across
/// processes* by the safety/motion/follow staleness checks, so the source must
/// be the same for every participant on a host, not a process-local origin. A
/// per-process monotonic `Instant` would make two processes' timestamps
/// incomparable (each counts from its own start), silently breaking freshness.
///
/// We therefore base `time_ns` on `SystemTime` (nanoseconds since the UNIX
/// epoch): a single host-wide domain every process agrees on, and the natural
/// fit for the simulation source that will publish wall-style time later. Wall
/// clocks can step backwards (NTP/manual set); within an epoch `time_ns` must
/// not regress, so the clock latches the last value and never reports a smaller
/// one. A real backward jump is the operator's signal to bump the epoch.
pub struct RealClock {
    epoch: u64,
    last_ns: Mutex<u64>,
}

impl RealClock {
    /// A real clock starting at epoch 0, in the host-wide UNIX-epoch domain.
    pub fn new() -> Self {
        RealClock {
            epoch: 0,
            last_ns: Mutex::new(0),
        }
    }

    /// Nanoseconds since the UNIX epoch, saturating at `u64::MAX` (year ~2554).
    fn unix_now_ns() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}

impl Default for RealClock {
    fn default() -> Self {
        RealClock::new()
    }
}

impl ClockSource for RealClock {
    fn now(&self) -> LogicalTime {
        let now = Self::unix_now_ns();
        // Latch monotonically within the epoch: a backward wall-clock step never
        // produces a smaller `time_ns` than already observed.
        let mut last = self.last_ns.lock().expect("real clock poisoned");
        *last = (*last).max(now);
        LogicalTime::new(self.epoch, *last)
    }
}

/// The simulation-time clock (the deferred "Webots port" source named in this
/// module's docs). In [`ClockMode::Simulation`](crate::participant::launch::ClockMode::Simulation)
/// the runner stamps `StepContext`/`produced_at_ns` from this clock instead of
/// [`RealClock`], so every simulated participant shares the *simulation* time
/// domain (owned by the Webots controller) rather than the host UNIX
/// domain. Cross-participant staleness checks (safety/motion/follow) then
/// compare sensor timestamps against the same sim clock the samples were
/// produced under; stamping sim participants from the wall clock instead makes
/// every sim-time sensor sample look arbitrarily stale and breaks those checks.
///
/// It reads the same authoritative logical time the
/// [`SimulationScheduler`](crate::participant::scheduler::SimulationScheduler)
/// releases ticks from - both share one [`watch`] channel driven by the live
/// `simulation/clock` feed - so "what time is it" and "when does the next
/// `#[step]` fire" never diverge. Before the first clock sample arrives it
/// reports the channel's seed (logical zero).
#[derive(Clone)]
pub struct SimulationClock {
    rx: watch::Receiver<LogicalTime>,
}

impl SimulationClock {
    /// Build a clock that observes `rx` - the receiver half of the same
    /// [`watch`] channel a
    /// [`SimulationScheduler`](crate::participant::scheduler::SimulationScheduler)
    /// is driven through, so both see identical logical time.
    pub(crate) fn from_receiver(rx: watch::Receiver<LogicalTime>) -> Self {
        Self { rx }
    }
}

impl ClockSource for SimulationClock {
    fn now(&self) -> LogicalTime {
        // The feed only ever advances the watched value (see
        // `SimulationClockHandle::advance`), so this is already monotonic within
        // an epoch and needs no latching of its own.
        *self.rx.borrow()
    }
}

/// An injectable fake clock for tests + the participant test harness (D34/D41).
#[derive(Clone)]
pub struct TestClock {
    state: Arc<Mutex<(u64, u64)>>, // (epoch, time_ns)
}

impl TestClock {
    /// A test clock at epoch 0, time 0.
    pub fn new() -> Self {
        TestClock {
            state: Arc::new(Mutex::new((0, 0))),
        }
    }

    /// Advance the current time by `delta`.
    pub fn advance(&self, delta: Duration) {
        let mut state = self.state.lock().expect("test clock poisoned");
        let ns = u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX);
        state.1 = state.1.saturating_add(ns);
    }

    /// Bump the epoch (reset) and restart time at 0.
    pub fn bump_epoch(&self) {
        let mut state = self.state.lock().expect("test clock poisoned");
        state.0 = state.0.saturating_add(1);
        state.1 = 0;
    }
}

impl Default for TestClock {
    fn default() -> Self {
        TestClock::new()
    }
}

impl ClockSource for TestClock {
    fn now(&self) -> LogicalTime {
        let state = self.state.lock().expect("test clock poisoned");
        LogicalTime::new(state.0, state.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_clock_uses_host_wide_unix_domain() {
        // Two independently constructed clocks model two processes on one host.
        // Because both read the shared UNIX-epoch domain (not a process-local
        // origin), their timestamps are directly comparable - the property the
        // cross-process staleness checks rely on (D34).
        let unix_before = RealClock::unix_now_ns();
        let a = RealClock::new();
        let b = RealClock::new();
        let ta = a.now().time_ns();
        let tb = b.now().time_ns();
        let unix_after = RealClock::unix_now_ns();

        assert!(
            ta >= unix_before && ta <= unix_after,
            "clock a ({ta}) is not in the UNIX-epoch domain [{unix_before}, {unix_after}]"
        );
        assert!(
            tb >= unix_before && tb <= unix_after,
            "clock b ({tb}) is not in the UNIX-epoch domain [{unix_before}, {unix_after}]"
        );
        // Comparable: both near "now", not separated by per-process origins.
        let gap = ta.abs_diff(tb);
        assert!(
            gap <= unix_after.saturating_sub(unix_before),
            "two host clocks disagree by {gap}ns, more than the sampling window"
        );
    }

    #[test]
    fn real_clock_never_regresses_within_epoch() {
        let clock = RealClock::new();
        let mut last = clock.now().time_ns();
        for _ in 0..1000 {
            let next = clock.now().time_ns();
            assert!(next >= last, "time_ns regressed: {next} < {last}");
            last = next;
        }
    }

    #[test]
    fn test_clock_is_deterministic() {
        let clock = TestClock::new();
        assert_eq!(clock.now(), LogicalTime::new(0, 0));
        clock.advance(Duration::from_nanos(5));
        assert_eq!(clock.now(), LogicalTime::new(0, 5));
        clock.advance(Duration::from_nanos(7));
        assert_eq!(clock.now(), LogicalTime::new(0, 12));
        clock.bump_epoch();
        assert_eq!(clock.now(), LogicalTime::new(1, 0));
    }
}
