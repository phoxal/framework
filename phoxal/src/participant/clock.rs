//! One type system, two clock drivers (#952 section I).
//!
//! The identity and coordinate authority is unified - every participant reads
//! [`RobotInstant`]s on one [`TimelineId`] - but the *tick mechanism*
//! deliberately is not:
//!
//! - **Real execution.** Cadence runs from the host's suspend-aware monotonic
//!   boot clock ([`LocalInstant`]) against a supervisor-minted execution
//!   origin. Control ticks never wait on a bus message: making real-mode
//!   cadence depend on a published clock would put the control loop behind a
//!   transport that is explicitly allowed to drop samples under saturation, and
//!   one-way published ticks cannot bound offset across hosts anyway.
//! - **Simulation and replay.** Exact discrete steps advanced by the world
//!   authority (the simulation controller). No interpolation. Pause means no
//!   new step; reset means a new timeline.
//!
//! # Losing clock discipline (#952 section J)
//!
//! A participant that cannot trust its clock does **not** freeze, and does not
//! wait to see whether the clock comes back. Freezing the steps is exactly the
//! failure mode that leaves an actuator commanded, and a grace window would be
//! an invented uncertainty bound: nothing in this design estimates how wrong an
//! untrustworthy clock is, so there is no honest threshold to wait out.
//!
//! Instead the clock reports [`ClockReading::Unsynchronized`] and the runner
//! fails the participant immediately. Teardown runs, so `Participant::shutdown` parks the
//! hardware; time-sensitive publication stops because the process stops; leases
//! and actuator permits stop being renewed, so the receiver-side deadlines and
//! the driver-local watchdogs stop the machine on their own clocks. The reason
//! travels in the failure, and the supervisor's ordinary restart and
//! start-limit policy decides what happens next - a transient fault recovers by
//! restarting with no retained state at all, and a persistently broken host
//! clock exhausts the start limit and stops the graph.
//!
//! A real participant whose clock is already untrustworthy at startup never
//! reaches its first step: it fails there, for the same reason.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;

use crate::bus::{LocalInstant, RobotInstant, TimelineId};
pub use phoxal_runtime_contract::{BootId, ExecutionOrigin};

/// Why a participant cannot currently produce a trustworthy robot instant.
///
/// These are the complete same-host v1 triggers. Transport loss is deliberately
/// **not** one of them: same-host robot time has no bus discipline feed, so a
/// dropped sample says nothing about the clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeUnsynchronized {
    /// The supervisor supplied no execution origin, or an unparsable one.
    #[error("the launch contract carried no valid execution origin")]
    MissingOrigin,
    /// The origin was minted against a different host boot, so it does not name
    /// an instant on this host's boot clock at all.
    #[error("the execution origin belongs to a different host boot")]
    ForeignBoot,
    /// The host clock could not be read, or read backwards.
    #[error("the host boot clock read failed or regressed")]
    ClockFault,
}

/// What a clock can currently say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockReading {
    /// A trustworthy instant on the participant's timeline.
    Synchronized(RobotInstant),
    /// The clock is not trustworthy, and why.
    Unsynchronized(TimeUnsynchronized),
}

impl ClockReading {
    /// The instant, if the clock is trustworthy.
    pub const fn instant(self) -> Option<RobotInstant> {
        match self {
            ClockReading::Synchronized(instant) => Some(instant),
            ClockReading::Unsynchronized(_) => None,
        }
    }
}

/// A source of robot time.
pub trait ClockSource: Send + Sync + 'static {
    /// The current reading. Within a timeline a synchronized reading never
    /// regresses.
    fn read(&self) -> ClockReading;
}

/// The real-execution clock: the host boot clock, offset by the execution
/// origin, projected onto the execution's timeline.
///
/// The domain is host-wide, so two processes on one host compute the same
/// [`RobotInstant`] for the same physical moment without exchanging a message.
/// Suspend counts, because [`LocalInstant`] reads the continuous clock.
pub struct RealClock {
    origin: Result<ExecutionOrigin, TimeUnsynchronized>,
    last_ticks: Mutex<u64>,
}

impl RealClock {
    /// A clock anchored at `origin`, validated against this host's boot.
    pub fn new(origin: ExecutionOrigin) -> Self {
        let origin = if origin.boot() == BootId::current() {
            Ok(origin)
        } else {
            Err(TimeUnsynchronized::ForeignBoot)
        };
        RealClock {
            origin,
            last_ticks: Mutex::new(0),
        }
    }

    /// A clock that reports [`TimeUnsynchronized::MissingOrigin`] until the
    /// supervisor supplies one.
    pub fn without_origin() -> Self {
        RealClock {
            origin: Err(TimeUnsynchronized::MissingOrigin),
            last_ticks: Mutex::new(0),
        }
    }
}

impl ClockSource for RealClock {
    fn read(&self) -> ClockReading {
        if LocalInstant::clock_faulted() {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        let origin = match self.origin {
            Ok(origin) => origin,
            Err(reason) => return ClockReading::Unsynchronized(reason),
        };
        let Some(now) = LocalInstant::try_now() else {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        };
        let started_at = LocalInstant::from_boot_ns(origin.boot_ns());
        if now < started_at {
            // The execution cannot have started after now on a clock that only
            // moves forward. Saturating to tick zero would silently place every
            // instant of this execution before its own origin.
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        let ticks =
            u64::try_from(now.saturating_duration_since(started_at).as_nanos()).unwrap_or(u64::MAX);
        // The boot clock is monotonic, so this is a defensive latch rather than
        // a correction: a regression means the clock read is untrustworthy.
        let mut last = self.last_ticks.lock().expect("real clock poisoned");
        if ticks < *last {
            return ClockReading::Unsynchronized(TimeUnsynchronized::ClockFault);
        }
        *last = ticks;
        ClockReading::Synchronized(RobotInstant::new(origin.timeline(), ticks))
    }
}

/// The simulation/replay clock: exact discrete steps advanced by the world
/// authority.
///
/// It reads the same authoritative instant the
/// [`SimulationScheduler`](crate::participant::scheduler::SimulationScheduler)
/// releases ticks from - both share one [`watch`] channel driven by the live
/// clock feed - so "what time is it" and "when does the next `Participant::step` fire"
/// never diverge. Before the first sample arrives there is no world history at
/// all, which is honestly reported as unsynchronized rather than as instant
/// zero of some invented timeline.
#[derive(Clone)]
pub struct SimulationClock {
    rx: watch::Receiver<Option<RobotInstant>>,
}

impl SimulationClock {
    /// Build a clock that observes `rx` - the receiver half of the same
    /// [`watch`] channel a
    /// [`SimulationScheduler`](crate::participant::scheduler::SimulationScheduler)
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
            None => ClockReading::Unsynchronized(TimeUnsynchronized::MissingOrigin),
        }
    }
}

/// An injectable deterministic clock for tests and the participant test
/// harness (D34/D41).
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
        self.state.lock().expect("test clock poisoned").0
    }

    /// Make every subsequent read report lost clock discipline, so a test can
    /// drive the failure path a real host only reaches by misbehaving.
    pub fn set_unsynchronized(&self, reason: TimeUnsynchronized) {
        *self.unsynchronized.lock().expect("test clock poisoned") = Some(reason);
    }

    /// Advance the current time by `delta`.
    pub fn advance(&self, delta: Duration) {
        let mut state = self.state.lock().expect("test clock poisoned");
        let ticks = u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX);
        state.1 = state.1.saturating_add(ticks);
    }

    /// Replace the world history (a reset) and restart at tick 0.
    pub fn replace_timeline(&self) -> TimelineId {
        let mut state = self.state.lock().expect("test clock poisoned");
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
        if let Some(reason) = *self.unsynchronized.lock().expect("test clock poisoned") {
            return ClockReading::Unsynchronized(reason);
        }
        let state = self.state.lock().expect("test clock poisoned");
        ClockReading::Synchronized(RobotInstant::new(state.0, state.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_clock_shares_one_host_wide_domain_across_processes() {
        // Two independently constructed clocks on one origin model two
        // processes on one host: because both read the host boot clock against
        // the same supervisor-minted origin, they compute directly comparable
        // instants - the property the cross-process freshness checks rely on.
        let origin = ExecutionOrigin::mint();
        let a = RealClock::new(origin);
        let b = RealClock::new(origin);
        let ta = a.read().instant().expect("clock a must be synchronized");
        let tb = b.read().instant().expect("clock b must be synchronized");
        assert_eq!(ta.timeline(), tb.timeline());
        let gap = tb
            .duration_since(ta)
            .expect("same timeline must be comparable");
        assert!(
            gap < Duration::from_secs(1),
            "two host clocks disagree by {gap:?}"
        );
    }

    #[test]
    fn execution_origins_and_local_instants_share_the_same_boot_clock_scale() {
        let before = LocalInstant::try_now().expect("test host clock");
        let origin = ExecutionOrigin::mint();
        let after = LocalInstant::try_now().expect("test host clock");

        assert!(
            before.boot_ns() <= origin.boot_ns() && origin.boot_ns() <= after.boot_ns(),
            "runtime-contract origin {} escaped the bus clock interval {}..={}",
            origin.boot_ns(),
            before.boot_ns(),
            after.boot_ns()
        );
    }

    #[test]
    fn the_real_clock_never_regresses_within_a_timeline() {
        let clock = RealClock::new(ExecutionOrigin::mint());
        let mut last = clock.read().instant().unwrap();
        for _ in 0..1000 {
            let next = clock.read().instant().unwrap();
            assert!(
                next.checked_cmp(last).unwrap() != std::cmp::Ordering::Less,
                "robot time regressed: {next} < {last}"
            );
            last = next;
        }
    }

    #[test]
    fn a_missing_or_foreign_boot_origin_is_reported_not_papered_over() {
        assert_eq!(
            RealClock::without_origin().read(),
            ClockReading::Unsynchronized(TimeUnsynchronized::MissingOrigin)
        );

        let foreign = ExecutionOrigin::new(
            BootId::from_raw(BootId::current().get() ^ 0xffff),
            LocalInstant::try_now().expect("test host clock").boot_ns(),
            TimelineId::mint(),
        );
        assert_eq!(
            RealClock::new(foreign).read(),
            ClockReading::Unsynchronized(TimeUnsynchronized::ForeignBoot)
        );
    }

    #[test]
    fn an_execution_origin_round_trips_through_the_launch_contract() {
        let origin = ExecutionOrigin::mint();
        assert_eq!(ExecutionOrigin::decode(&origin.encode()), Some(origin));
        assert_eq!(ExecutionOrigin::decode("garbage"), None);
        assert_eq!(
            ExecutionOrigin::decode("1:2:0"),
            None,
            "timeline zero is not a timeline"
        );
        assert_eq!(ExecutionOrigin::decode("1:2:3:4"), None);
    }

    #[test]
    fn the_boot_identity_is_stable_within_one_boot() {
        assert_eq!(BootId::current(), BootId::current());
    }

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
