// Declaring a subscribe handle does not permit publishing that family.
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "direction-mismatch", api = y2026_1)]
struct DirectionMismatch {
    target: Latest<api::drive::Target>,
}

#[phoxal::runtime]
impl DirectionMismatch {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _publish = ctx.publisher(api::topic::new().drive().target()).await?;
        Ok(Self {
            target: ctx
                .subscribe(api::topic::new().drive().target())
                .latest()
                .await?,
        })
    }
}

fn main() {}
