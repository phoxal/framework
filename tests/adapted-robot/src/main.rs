//! A local pose fixture for the independently built World and Navigation
//! services. This one executable hosts the authored brain runtime and the
//! generated conversion role together: preparation persists the discovered
//! edges, the build helper emits the hosting glue into `OUT_DIR`, and the
//! supervisor launches the same binary once per instance id. The authored
//! conversion module reads generated bindings, so it compiles only once
//! the package's own prepared products exist.

#[cfg(phoxal_self_prepared)]
mod conversions;

phoxal::api!();
phoxal::conversions!();

use phoxal::contracts::Latest;
use phoxal::runtime::{InitContext, Runtime, StepContext};

/// The brain's own expectation of the latest world revision.
#[phoxal::message]
pub struct BrainWorldStatus {
    #[phoxal(tag = 1)]
    pub revision: u64,
    #[phoxal(tag = 2)]
    pub available: bool,
}

/// The brain's empty endpoint contract.
#[phoxal::endpoints]
pub struct BrainApi {
    #[phoxal::input(max_age_ms = 100, max_bytes = 128)]
    world_status: Latest<BrainWorldStatus>,

    #[phoxal::output(projection = state, bootstrap, max_bytes = 16)]
    heartbeat: Latest<::phoxal::contracts::Empty>,

    #[phoxal::output(projection = state, bootstrap, max_bytes = 512)]
    odometry: Latest<::phoxal::contracts::robotics::OdometryState>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Brain;

impl brain_api::projections::Projections for Brain {
    type State = ::phoxal::contracts::robotics::OdometryState;

    fn heartbeat(&self, _state: &Self::State) -> ::phoxal::contracts::Empty {
        ::phoxal::contracts::Empty::default()
    }

    fn odometry(&self, state: &Self::State) -> ::phoxal::contracts::robotics::OdometryState {
        state.clone()
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
        _ctx: &StepContext,
        mut state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.revision = state.revision.saturating_add(1);
        state.oldest_capture_time_nanos = Some(_ctx.now().as_nanos());
        Ok((state, Self::Outputs::default()))
    }
}

fn main() -> phoxal::Result<()> {
    run_hosted_roles(Brain)
}
