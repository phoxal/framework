//! Static metadata markers the macros target (D50/D59/D60).
//!
//! - [`TypedGraphSurface`] is emitted by `#[phoxal::service|driver|simulator]`
//!   and gates the typed graph handle builders away from thin tool runners.
//! - [`SchedulableSurface`] is emitted by `#[phoxal::service|driver]` and gates
//!   scheduled steps away from clockless tools and simulators.
//! - [`IsDriver`]/[`IsSimulator`]/[`IsTool`] are emitted by the matching
//!   attribute macro and gate the kind-specific `SetupContext` accessors
//!   (`component()`, `raw_bus()`, and - for [`IsSimulator`] - world-clock
//!   authority) in `participant::api`. All three are sealed (see
//!   [`sealing::Sealed`]): ordinary participant code cannot write
//!   `impl IsSimulator for MyType` and unlock those accessors on a type the
//!   matching macro never blessed.
//! - [`StepSchedule`]/[`MissedTick`] describe a `#[step(hz = …)]` loop's cadence
//!   and overrun policy; the runner ([`participant::runner`](super::runner))
//!   reads them from
//!   [`Participant::__step_schedule`](super::api::Participant::__step_schedule).

use std::time::Duration;

/// The sealing boundary for [`IsDriver`]/[`IsSimulator`]/[`IsTool`].
///
/// This module is `pub` - not truly private - because the marker impls it
/// gates are written by `#[phoxal::driver|simulator|tool]`'s macro expansion,
/// which runs in the *downstream* participant crate, not this one. Rust has
/// no visibility level between "this crate" and "the world" that a
/// cross-crate macro expansion could still reach, so this cannot be a true
/// `pub(crate)` seal. It is `#[doc(hidden)]` and named so a participant would
/// have to go out of their way to find and write this exact path; a
/// participant that does can still `impl sealing::Sealed for MyType {}` and
/// then `impl IsSimulator for MyType {}` by hand. That closes the
/// *accidental* route (writing `impl IsSimulator for MyType` alone no longer
/// compiles), not a sealed capability boundary.
#[doc(hidden)]
pub mod sealing {
    /// See the [module docs](self) for the exact strength of this seal.
    pub trait Sealed {}
}

/// Marker emitted only by participant roles that expose typed graph handles.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a tool, which is a thin raw-bus runner and has no typed-graph surface",
    label = "typed graph handles are not available here; use the raw bus (`phoxal::raw`) instead"
)]
pub trait TypedGraphSurface {}

/// Marker emitted only by participant roles whose launch policy carries a
/// schedulable robot clock.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` has a clockless launch policy and cannot own a scheduled step",
    label = "scheduled steps are available only on services and drivers"
)]
pub trait SchedulableSurface {}

/// Marker emitted only by `#[phoxal::driver]`. Sealed - see [`sealing`].
#[doc(hidden)]
pub trait IsDriver: sealing::Sealed {}

/// Marker emitted only by `#[phoxal::tool]`. Sealed - see [`sealing`].
#[doc(hidden)]
pub trait IsTool: sealing::Sealed {}

/// Marker emitted only by `#[phoxal::simulator]`. Sealed - see [`sealing`]:
/// this is what keeps an arbitrary participant from unlocking the
/// `SetupContextSimulatorExt` world-clock accessors
/// (`phoxal::participant::api`) by writing `impl IsSimulator for MyType {}`
/// on its own.
#[doc(hidden)]
pub trait IsSimulator: sealing::Sealed {}

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
    use crate::prelude::*;

    #[test]
    fn step_schedule_period_never_rounds_to_zero() {
        assert_eq!(StepSchedule::hz(f64::MAX).period(), Duration::from_nanos(1));
    }

    #[phoxal::simulator(id = "marker-simulator")]
    struct MarkerSimulator;

    impl Participant for MarkerSimulator {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::tool(id = "marker-tool")]
    struct MarkerTool;

    impl Participant for MarkerTool {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    /// Each kind macro emits its own marker, which is what gates the
    /// kind-specific `SetupContext` accessors. A tool additionally has no
    /// typed-graph surface.
    #[test]
    fn kind_macros_emit_their_markers() {
        fn assert_simulator<T: IsSimulator + TypedGraphSurface>() {}
        fn assert_tool<T: IsTool>() {}

        assert_simulator::<MarkerSimulator>();
        assert_tool::<MarkerTool>();
    }
}
