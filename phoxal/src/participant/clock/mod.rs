//! One type system, two clock drivers.
//!
//! The identity and coordinate authority is unified - every participant reads
//! [`RobotInstant`](crate::bus::RobotInstant)s on one
//! [`TimelineId`](crate::bus::TimelineId) - but the *tick mechanism*
//! deliberately is not:
//!
//! - **Real execution.** Robot time zero is host boot, so a real reading is the
//!   host's suspend-aware monotonic boot clock
//!   ([`LocalInstant`](crate::bus::LocalInstant)) read straight onto the
//!   execution's timeline - there is no origin to anchor against and nothing to
//!   distribute. Control ticks never wait on a bus message: making real-mode
//!   cadence depend on a published clock would put the control loop behind a
//!   transport that is explicitly allowed to drop samples under saturation, and
//!   one-way published ticks cannot bound offset across hosts anyway.
//! - **Simulation and replay.** Exact discrete steps advanced by the world
//!   authority (the simulation controller). No interpolation. Pause means no
//!   new step; reset means a new timeline.
//!
//! # Losing clock discipline
//!
//! A participant that cannot trust its clock does **not** freeze, and does not
//! wait to see whether the clock comes back. Freezing the steps is exactly the
//! failure mode that leaves an actuator commanded, and a grace window would be
//! an invented uncertainty bound: nothing in this design estimates how wrong an
//! untrustworthy clock is, so there is no honest threshold to wait out.
//!
//! Instead the clock reports [`ClockReading::Unsynchronized`] and the runner
//! fails the participant immediately. Teardown runs, so `Participant::shutdown`
//! parks the hardware; time-sensitive publication stops because the process
//! stops; leases and actuator permits stop being renewed, so the receiver-side
//! deadlines and the driver-local watchdogs stop the machine on their own
//! clocks. The reason travels in the failure, and the supervisor's ordinary
//! restart and start-limit policy decides what happens next - a transient fault
//! recovers by restarting with no retained state at all, and a persistently
//! broken host clock exhausts the start limit and stops the graph.
//!
//! A real participant whose clock is already untrustworthy at startup never
//! reaches its first step: it fails there, for the same reason.

use crate::bus::RobotInstant;

pub(crate) mod real;
pub(crate) mod simulation;
// Reached only through `crate::testing`, which the `test-harness` profile
// declares; the module itself is compiled in every profile so that no domain
// module has to know which profile it is in.
#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
pub(crate) mod test;

/// Why a participant cannot currently produce a trustworthy robot instant.
///
/// One trigger per clock. The origin-shaped failures went with the origin:
/// real robot time is now the
/// host boot clock itself, so there is no supplied anchor to be missing and no
/// minted boot identity for a reading to disagree with - a process reads its
/// clock or it does not. Transport loss is deliberately not a trigger either:
/// same-host robot time has no bus discipline feed, so a dropped sample says
/// nothing about the clock.
///
/// `pub` because it is named by [`ClockReading`], which [`ClockSource::read`]
/// returns and the in-process runner seam therefore exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeUnsynchronized {
    /// The host clock could not be read, or read backwards.
    #[error("the host boot clock read failed or regressed")]
    ClockFault,
    /// A simulated participant's world authority has not published a first step
    /// yet, so there is no world history to date anything on. This is a world
    /// that has not started rather than a clock that was lost, which is why the
    /// runner's recurring beat deliberately does not fault on it.
    #[error("the simulated world authority has published no step yet")]
    NoWorldHistory,
}

/// Which clock a launched participant runs on.
///
/// The launch contract's `--simulation` flag is the whole of this decision:
/// simulation is a launcher
/// choice, never a bundle fact, and there is no third mode. A real participant
/// that declares no `#[phoxal::step]` simply never steps; it does not become a
/// different kind of participant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClockMode {
    /// Host boot clock, on the execution's timeline.
    Real,
    /// The world clock published on `runtime/simulation/clock`.
    Simulation,
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
