//! The participant lifecycle authoring model.
//!
//! Role attributes declare static identity and `Config`/`State`/`Api` types;
//! authors implement lifecycle behavior directly. Configuration schema
//! composition, static participant specification, and typed query dispatch
//! live in their own concept-owned modules and are re-exported here for the
//! engine's existing internal paths.

use super::spec::ParticipantSpec;
use crate::participant::context::{ResetContext, SetupContext, StepContext};
use crate::participant::scheduler::StepSchedule;

/// Participant lifecycle behavior.
///
/// One runner task owns `State` and serializes step, query, reset, and
/// shutdown access. `Api` is separate and shared immutably with behavior.
///
/// `setup` and `shutdown` are asynchronous because they acquire and release
/// external resources. Scheduled `step` and timeline `reset` transitions are
/// deliberately synchronous: they consume the snapshots and bounded queues
/// already admitted by setup-owned tasks, so an external dependency cannot
/// hold the runner's scheduler hostage.
// `allow` rather than `expect`: the lint only fires for a publicly reachable
// trait, and only the `participant` profile publishes this one.
#[allow(
    async_fn_in_trait,
    reason = "setup and shutdown are awaited directly by the runner; scheduled state transitions remain synchronous"
)]
pub trait Participant: ParticipantSpec {
    /// Build initial mutable state and bus-facing handles.
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> crate::Result<(Self::State, Self::Api)>;

    /// Run one scheduled step.
    fn step(
        &self,
        _api: &Self::Api,
        _step: StepContext,
        _state: &mut Self::State,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Reset state derived from a replaced simulation timeline.
    fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        _state: &mut Self::State,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Gracefully park, stop, flush, or publish a final externally observable
    /// action before the bus closes.
    ///
    /// Override this only when teardown has an effect outside the state that
    /// the runner is about to drop. Ordinary participants omit it. The runner
    /// bounds the hook with the launch contract's shutdown grace period.
    async fn shutdown(&self, _api: &Self::Api, _state: &mut Self::State) -> crate::Result<()> {
        Ok(())
    }

    /// Cadence emitted alongside a `#[phoxal::step(hz = N)]` override.
    #[doc(hidden)]
    fn __step_schedule() -> Option<StepSchedule> {
        None
    }
}
