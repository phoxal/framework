//! The clock + time source (D34).
//!
//! No runtime mints its own clock; the runner owns one [`ClockSource`] and stamps
//! every `StepContext`/`produced_at_ns` from it, so all participants share one
//! logical-time domain. Three sources are envisaged: **real** (host-monotonic),
//! **simulation** (subscribe the authoritative `simulation/clock`), and **test**
//! (an injectable fake). The first slice ships real + test; the simulation source
//! lands with the Webots port.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::bus::LogicalTime;

/// A source of logical robot time.
pub trait ClockSource: Send + Sync + 'static {
    /// The current logical time. Within an epoch this strictly increases.
    fn now(&self) -> LogicalTime;
}

/// Host-monotonic clock.
///
/// The slice bases time on a process-local monotonic `Instant`; aligning the
/// monotonic domain across processes on one host (`CLOCK_MONOTONIC` absolute) is
/// a documented follow-up (D34, "host-monotonic domain shared across processes").
pub struct RealClock {
    epoch: u64,
    start: Instant,
}

impl RealClock {
    /// A real clock starting at epoch 0.
    pub fn new() -> Self {
        RealClock {
            epoch: 0,
            start: Instant::now(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        RealClock::new()
    }
}

impl ClockSource for RealClock {
    fn now(&self) -> LogicalTime {
        let ns = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        LogicalTime::new(self.epoch, ns)
    }
}

/// An injectable fake clock for tests + the runtime test harness (D34/D41).
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
