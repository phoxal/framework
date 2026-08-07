//! The deterministic clock a test injects in place of the host clock.
//!
//! This module exists only under `cfg(test)` or the `test-harness` feature. A
//! participant crate's tests are a separate compilation unit that cannot enable
//! this crate's `cfg(test)`, so a downstream crate wanting to drive a
//! participant on a clock it controls declares
//! `phoxal = { features = ["test-harness"] }` as a **dev**-dependency; nothing
//! in a shipped participant binary can reach it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::bus::{RobotInstant, TimelineId};
use crate::participant::lock;

/// An injectable deterministic clock: time moves only when a test advances it,
/// and lost clock discipline is something a test asks for rather than something
/// it has to provoke a real host into.
#[derive(Clone)]
pub struct TestClock {
    state: Arc<Mutex<(TimelineId, u64)>>,
    unsynchronized: Arc<Mutex<Option<TimeUnsynchronized>>>,
}

impl TestClock {
    /// A test clock at tick 0 on a fresh timeline.
    pub fn new() -> Self {
        TestClock {
            state: Arc::new(Mutex::new((TimelineId::mint(), 0))),
            unsynchronized: Arc::new(Mutex::new(None)),
        }
    }

    /// The timeline this clock is currently on.
    pub fn timeline(&self) -> TimelineId {
        lock(&self.state).0
    }

    /// Make every subsequent read report lost clock discipline, so a test can
    /// drive the failure path a real host only reaches by misbehaving.
    pub fn set_unsynchronized(&self, reason: TimeUnsynchronized) {
        *lock(&self.unsynchronized) = Some(reason);
    }

    /// Advance the current time by `delta`.
    pub fn advance(&self, delta: Duration) {
        let mut state = lock(&self.state);
        let ticks = u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX);
        state.1 = state.1.saturating_add(ticks);
    }

    /// Replace the world history (a reset) and restart at tick 0.
    pub fn replace_timeline(&self) -> TimelineId {
        let mut state = lock(&self.state);
        state.0 = TimelineId::mint();
        state.1 = 0;
        state.0
    }
}

impl Default for TestClock {
    fn default() -> Self {
        TestClock::new()
    }
}

impl ClockSource for TestClock {
    fn read(&self) -> ClockReading {
        if let Some(reason) = *lock(&self.unsynchronized) {
            return ClockReading::Unsynchronized(reason);
        }
        let state = lock(&self.state);
        ClockReading::Synchronized(RobotInstant::new(state.0, state.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_clock_is_deterministic_and_resets_onto_a_new_timeline() {
        let clock = TestClock::new();
        let first = clock.timeline();
        assert_eq!(
            clock.read(),
            ClockReading::Synchronized(RobotInstant::new(first, 0))
        );
        clock.advance(Duration::from_nanos(5));
        clock.advance(Duration::from_nanos(7));
        assert_eq!(
            clock.read(),
            ClockReading::Synchronized(RobotInstant::new(first, 12))
        );

        let second = clock.replace_timeline();
        assert_ne!(second, first);
        assert_eq!(
            clock.read(),
            ClockReading::Synchronized(RobotInstant::new(second, 0))
        );
    }
}
