//! Sealed role capabilities emitted by participant macros plus step scheduling
//! metadata consumed by the runtime engine.

use std::time::Duration;

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
    use crate::__private::surface::{
        ComponentBoundSurface, ToolSurface, TypedIoSurface, WorldAuthoritySurface,
    };
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
        fn assert_simulator<T: WorldAuthoritySurface + ComponentBoundSurface + TypedIoSurface>() {}
        fn assert_tool<T: ToolSurface>() {}

        assert_simulator::<MarkerSimulator>();
        assert_tool::<MarkerTool>();
    }
}
