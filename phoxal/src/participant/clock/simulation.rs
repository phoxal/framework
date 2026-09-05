//! The simulation/replay clock.

use tokio::sync::watch;

use super::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::bus::RobotInstant;

/// The simulation/replay clock: exact discrete steps advanced by an external
/// logical-time source.
///
/// It reads the same authoritative instant the
/// [`SimulationScheduler`](crate::participant::scheduler::simulation::SimulationScheduler)
/// releases ticks from - both share one [`watch`] channel driven by the live
/// logical-time feed - so "what time is it" and "when does the next `Participant::step`
/// fire" never diverge. Before the first sample arrives there is no world
/// history at all, which is honestly reported as unsynchronized rather than as
/// instant zero of some invented timeline.
#[derive(Clone)]
pub(crate) struct SimulationClock {
    rx: watch::Receiver<Option<RobotInstant>>,
}

impl SimulationClock {
    /// Build a clock that observes `rx` - the receiver half of the same
    /// [`watch`] channel a
    /// [`SimulationScheduler`](crate::participant::scheduler::simulation::SimulationScheduler)
    /// is driven through, so both see identical robot time.
    pub(crate) fn from_receiver(rx: watch::Receiver<Option<RobotInstant>>) -> Self {
        Self { rx }
    }
}

impl ClockSource for SimulationClock {
    fn read(&self) -> ClockReading {
        // The feed only ever advances the watched value (see
        // `SimulationClockHandle::advance`), so this is already monotonic
        // within a timeline and needs no latching of its own.
        match *self.rx.borrow() {
            Some(instant) => ClockReading::Synchronized(instant),
            None => ClockReading::Unsynchronized(TimeUnsynchronized::NoWorldHistory),
        }
    }
}
