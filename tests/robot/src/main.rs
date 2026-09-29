//! Disarmed default brain for the internal qualification robot: the leased
//! manual intent's type is generated from the controller's prepared
//! compiled contract and carries the same schema and codec contract as an
//! authored message; the brain owns only the disarmed projection.

#[cfg(phoxal_self_prepared)]
mod conversions;

use phoxal::contracts::Latest;
use phoxal::runtime::{InitContext, Runtime, StepContext};
phoxal::api!();
phoxal::conversions!();

/// The brain's endpoint contract: one leased manual projection over the
/// controller's generated payload type.
#[phoxal::endpoints]
pub struct BrainApi {
    #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 256)]
    manual: Latest<crate::api::types::phoxal::motion::v1::MotionIntent>,
    #[phoxal::output(projection = state, bootstrap, max_bytes = 512)]
    odometry: Latest<::phoxal::contracts::robotics::OdometryState>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Brain;

impl brain_api::projections::Projections for Brain {
    type State = ::phoxal::contracts::robotics::OdometryState;

    /// The normal robot starts stopped.
    /// The local scenario substitutes the controller's manual input without
    /// turning the qualification program into deployed robot behavior.
    fn manual(
        &self,
        _state: &Self::State,
    ) -> Option<crate::api::types::phoxal::motion::v1::MotionIntent> {
        None
    }

    fn odometry(&self, state: &Self::State) -> ::phoxal::contracts::robotics::OdometryState {
        *state
    }
}

#[phoxal::runtime(contract = BrainApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Brain {
    type Config = ();
    type State = ::phoxal::contracts::robotics::OdometryState;

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> {
        Ok(Self::State {
            available: true,
            ..Self::State::default()
        })
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.revision = state.revision.saturating_add(1);
        state.oldest_capture_time_nanos = Some(ctx.now().as_nanos());
        Ok((state, Self::Outputs::default()))
    }
}

fn main() -> phoxal::Result<()> {
    run_hosted_roles(Brain)
}
