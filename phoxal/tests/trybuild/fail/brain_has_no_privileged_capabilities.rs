// The brain owns policy, not privilege: it is neither component-bound (a
// driver/simulator capability) nor a world-clock authority (a simulator
// capability).
use phoxal::api;
use phoxal::prelude::*;

#[phoxal::brain]
struct PrivilegedBrain;

impl Participant for PrivilegedBrain {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _component = ctx.component()?;
        let _authority = ctx.timeline_authority(TimelineId::mint())?;
        let _clock = ctx
            .world_clock_publisher(api::topic::owner().simulation().clock())?;
        Ok(((), ()))
    }
}

fn main() {}
