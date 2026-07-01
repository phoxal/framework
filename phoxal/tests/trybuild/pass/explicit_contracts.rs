// setup-only IO can be declared with #[phoxal(contracts(...))].
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(
    id = "setup-only",
    api = y2026_1,
    contracts(
        publishes(api::drive::Target),
        subscribes(api::drive::State),
        queries(api::map::SubmapRequest => api::map::SubmapResponse),
    )
)]
struct SetupOnly {}

#[phoxal::behavior]
impl SetupOnly {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _target = ctx.publisher(api::topic::new().drive().target()).await?;
        let _state = ctx
            .subscribe_with(
                api::topic::new().drive().state(),
                phoxal::participant::SubscribeOptions::new().depth(8),
            )
            .latest()
            .await?;
        let _submap = ctx.querier(api::topic::new().map().submap()).await?;
        Ok(Self {})
    }
}

fn main() {}
