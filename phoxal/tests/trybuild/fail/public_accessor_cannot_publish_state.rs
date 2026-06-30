// L1 (plan #00): taking the WRONG side of a topic is a compile error.
//
// `drive/state` is a `state` topic. The owner (the `drive` runtime) publishes it;
// a client only observes it. The PUBLIC `topic::new()...` chain is the CLIENT side,
// so `api::topic::new().drive().state()` yields a `Topic<Subscribe<drive::State>>`.
// Feeding that to `ctx.publisher` - which requires `Topic<Publish<B>>` - is a type
// error: the public accessor simply cannot hand a publisher of someone else's
// state. The owner side is reachable only through the deliberate, greppable
// `api::topic::internal::new()...` builder, whose `state` leaf is `Publish`.
//
// This struct declares the publish handle, so `R: DeclaresPublish<drive::State>`
// is satisfied and the SOLE remaining error is the side-brand mismatch on the
// public accessor - exactly the L1 guarantee.
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "public-publish-state", api = y2026_1)]
struct PublicPublishState {
    state: Publisher<api::drive::State>,
}

#[phoxal::runtime]
impl PublicPublishState {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // ERROR: the public (client) `state` leaf is `Topic<Subscribe<drive::State>>`,
        // but `publisher` takes `Topic<Publish<B>>`. Publishing a `state` requires
        // the owner builder: `api::topic::internal::new().drive().state()`.
        let state = ctx.publisher(api::topic::new().drive().state()).await?;
        Ok(Self { state })
    }
}

fn main() {}
