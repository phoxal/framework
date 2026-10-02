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
use phoxal::runtime::Context;

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

#[derive(Clone, Debug, Default)]
struct Brain {
    odometry: ::phoxal::contracts::robotics::OdometryState,
}

#[phoxal::runtime(contract = BrainApi, period_ms = 20)]
impl Brain {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self {
            odometry: ::phoxal::contracts::robotics::OdometryState {
                available: true,
                ..Default::default()
            },
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.odometry.revision = self.odometry.revision.saturating_add(1);
        self.odometry.oldest_capture_time_nanos = Some(ctx.now().as_nanos());
        Ok(())
    }

    #[publish(heartbeat)]
    fn heartbeat(&self) -> ::phoxal::contracts::Empty {
        ::phoxal::contracts::Empty::default()
    }

    #[publish(odometry)]
    fn odometry_projection(&self) -> ::phoxal::contracts::robotics::OdometryState {
        self.odometry.clone()
    }
}

fn main() -> phoxal::Result<()> {
    run_hosted_roles(phoxal_runtime_brain::Adapter::new())
}
