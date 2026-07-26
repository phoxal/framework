// L1 (plan #00): taking the WRONG side of a topic is a compile error.
//
// `drive/state` is a `state` topic. The owner (the `drive` participant) publishes it;
// a client only observes it. The PUBLIC `topic::client()...` chain is the CLIENT side,
// so `api::topic::client().drive().state()` yields a `Topic<Subscribe<drive::State>>`.
// Feeding that to `ctx.publisher` - which requires `Topic<Publish<B>>` - is a type
// error: the public accessor simply cannot hand a publisher of someone else's
// state. The owner side is reachable only through the deliberate, greppable
// `api::topic::owner()...` builder, whose `state` leaf is `Publish`.
//
// The SOLE error is the side-brand mismatch on the public accessor - exactly the
// L1 guarantee.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    state: StatePublisher<api::drive::State>,
}

#[phoxal::service(id = "public-publish-state")]
struct PublicPublishState;

#[phoxal::behavior]
impl PublicPublishState {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: the public (client) `state` leaf is `Topic<Subscribe<drive::State>>`,
        // but `publisher` takes `Topic<Publish<B>>`. Publishing a `state` requires
        // the owner builder: `api::topic::owner().drive().state()`.
        let state = ctx.state_publisher(api::topic::client().drive().state()).await?;
        Ok((Self, Self::Api { state }))
    }
}

fn main() {}
