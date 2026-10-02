//! Disarmed default brain for the internal qualification robot: the leased
//! manual intent's type is generated from the controller's prepared
//! compiled contract and carries the same schema and codec contract as an
//! authored message; the brain owns only the disarmed projection.

#[cfg(phoxal_self_prepared)]
mod conversions;

use phoxal::contracts::Latest;
use phoxal::runtime::Context;
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

    /// The normal robot starts stopped.
    /// The local scenario substitutes the controller's manual input without
    /// turning the qualification program into deployed robot behavior.
    #[publish(manual)]
    fn manual(&self) -> Option<crate::api::types::phoxal::motion::v1::MotionIntent> {
        None
    }

    #[publish(odometry)]
    fn odometry_projection(&self) -> ::phoxal::contracts::robotics::OdometryState {
        self.odometry
    }
}

fn main() -> phoxal::Result<()> {
    run_hosted_roles(phoxal_runtime_brain::Adapter::new())
}
