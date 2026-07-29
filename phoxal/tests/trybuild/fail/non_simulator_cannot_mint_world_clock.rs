use phoxal::api;
use phoxal::prelude::*;

#[phoxal::service(id = "not-a-simulator")]
struct NotASimulator;

impl Participant for NotASimulator {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _authority = ctx.timeline_authority(TimelineId::mint())?;
        let _clock = ctx
            .world_clock_publisher(api::topic::owner().simulation().clock())
            .await?;
        Ok(((), ()))
    }
}

fn main() {}
