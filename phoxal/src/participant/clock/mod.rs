//! One type system, two clock drivers.
//!
//! The identity and coordinate authority is unified - every participant reads
//! [`RobotInstant`](crate::bus::RobotInstant)s on one
//! [`TimelineId`](crate::bus::TimelineId) - but the *tick mechanism*
//! deliberately is not:
//!
//! - **Real execution.** Cadence runs from the host's suspend-aware monotonic
//!   boot clock ([`LocalInstant`](crate::bus::LocalInstant)) against a
//!   supervisor-minted execution origin. Control ticks never wait on a bus
//!   message: making real-mode cadence depend on a published clock would put the
//!   control loop behind a transport that is explicitly allowed to drop samples
//!   under saturation, and one-way published ticks cannot bound offset across
//!   hosts anyway.
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
#[cfg(feature = "test-harness")]
pub mod test;

/// Why a participant cannot currently produce a trustworthy robot instant.
///
/// These are the complete same-host triggers. Transport loss is deliberately
/// **not** one of them: same-host robot time has no bus discipline feed, so a
/// dropped sample says nothing about the clock.
/// `pub` because it is named by [`ClockReading`], which [`ClockSource::read`]
/// returns and the in-process runner seam therefore exposes.
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
