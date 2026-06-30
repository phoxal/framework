// Declaring a subscribe handle does not permit publishing that family. The struct
// declares only a `drive::Target` *subscribe* handle (a `Latest`), so the
// `R: DeclaresPublish<drive::Target>` gate on `ctx.publisher` is unsatisfied even
// though the side brand lines up: `drive/target` is a `command`, so the public
// builder hands a client `Topic<Publish<Target>>` that `publisher` would accept on
// the brand alone. The valid declared field is built through the owner (`internal`)
// builder, whose `command` leaf is the matching `Topic<Subscribe<Target>>`, so the
// ONLY error is the unsatisfied publish declaration (not a side-brand mismatch).
use phoxal_api::y2026_1 as api;
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
                .subscribe(api::topic::internal::new().drive().target())
                .latest()
                .await?,
        })
    }
}

fn main() {}
