// L1 (plan #00): the owner reaches its publish/subscribe/serve side through the
// deliberate `api::topic::internal::new()...` builder, while a client reaches the
// inverse sides through the public `api::topic::new()...` builder. This fixture is
// the positive counterpart to `fail/public_accessor_cannot_publish_state.rs`: it
// must COMPILE.
//
// `Owner` is the `drive` owner: it subscribes its `command` input and publishes its
// `state` telemetry, both via the owner builder. `Client` is a `drive` consumer: it
// publishes the `command` (sends a target) and subscribes the `state`, both via the
// public builder. Either side reaching for the other's brand would fail to compile.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct OwnerApi {
    target: Subscriber<api::drive::Target>,
    state: Publisher<api::drive::State>,
}

#[phoxal::service(id = "drive-owner", api = OwnerApi)]
struct Owner;

#[phoxal::behavior]
impl Owner {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability the owner
        // (`internal`) builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self,
            Self::Api {
                // Owner side: `command` -> Subscribe, `state` -> Publish.
                target: ctx
                    .subscriber(api::topic::internal::new(cap).drive().target(), 32)
                    .await?,
                state: ctx
                    .publisher(api::topic::internal::new(cap).drive().state())
                    .await?,
            },
        ))
    }
}

#[derive(phoxal::Api)]
struct ClientApi {
    target: Publisher<api::drive::Target>,
    state: Latest<api::drive::State>,
}

#[phoxal::service(id = "drive-client", api = ClientApi)]
struct Client;

#[phoxal::behavior]
impl Client {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                // Client side: `command` -> Publish, `state` -> Subscribe.
                target: ctx.publisher(api::topic::new().drive().target()).await?,
                state: ctx.latest(api::topic::new().drive().state()).await?,
            },
        ))
    }
}

fn main() {}
