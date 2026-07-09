//! Static metadata markers the macros target (D50/D59/D60).
//!
//! - [`TypedGraphSurface`] is emitted by `#[phoxal::service|driver|simulator]`
//!   and gates `#[step]` / `#[server]` / `#[server_snapshot]` away from thin
//!   `#[phoxal::tool]` runners.
//! - [`IsDriver`]/[`IsSimulator`]/[`IsTool`] are emitted by the matching
//!   attribute macro and gate the kind-specific `SetupContext` accessors
//!   (`component()`, `raw_bus()`) in `participant::api`.
//! - [`StepSchedule`]/[`MissedTick`] describe a `#[step(hz = …)]` loop's cadence
//!   and overrun policy; the runner ([`participant::runner`](super::runner))
//!   reads them from
//!   [`ParticipantLifecycle::__step_schedule`](super::api::ParticipantLifecycle::__step_schedule).

use std::time::Duration;

/// Marker emitted only by checked participant macros that expose the typed graph
/// surface (`#[step]` / `#[server]` / `#[server_snapshot]`).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a tool, which is a thin raw-bus runner and has no typed-graph surface",
    label = "`#[step]` / `#[server]` is not allowed here; use the raw bus (`phoxal::raw`) instead"
)]
pub trait TypedGraphSurface {}

/// Marker emitted only by `#[phoxal::driver]`.
#[doc(hidden)]
pub trait IsDriver {}

/// Marker emitted only by `#[phoxal::tool]`.
#[doc(hidden)]
pub trait IsTool {}

/// Marker emitted only by `#[phoxal::simulator]`.
#[doc(hidden)]
pub trait IsSimulator {}

/// The cadence + missed-tick policy of a `#[step(hz = …)]` loop (D34).
#[derive(Clone, Copy, Debug)]
pub struct StepSchedule {
    /// Target frequency in Hz.
    pub hz: f64,
    /// What to do after an overrun.
    pub missed_tick: MissedTick,
}

impl StepSchedule {
    /// A schedule at `hz` with the default (`Collapse`) missed-tick policy.
    pub const fn hz(hz: f64) -> Self {
        StepSchedule {
            hz,
            missed_tick: MissedTick::Collapse,
        }
    }

    /// The nominal step period.
    pub fn period(&self) -> Duration {
        std::cmp::max(
            Duration::from_secs_f64(1.0 / self.hz),
            Duration::from_nanos(1),
        )
    }
}

/// Missed-tick policy after a step overruns its period (D34).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissedTick {
    /// Run a single step after an overrun and record `missed_ticks`; no burst
    /// catch-up. The v1 default.
    Collapse,
    /// Replay every missed tick. Reserved for offline replay.
    CatchUp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_schedule_period_never_rounds_to_zero() {
        assert_eq!(StepSchedule::hz(f64::MAX).period(), Duration::from_nanos(1));
    }
}
