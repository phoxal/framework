// The positive counterpart to `fail/undeclared_subscribe_handle.rs`: declaring
// the `Latest<drive::State>` field on `Api` satisfies `Self::Api:
// DeclaresSubscribe<drive::State>`, so the identical `ctx.latest(...)` call
// compiles.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: CommandPublisher<api::drive::Target>,
    state: Latest<api::drive::State>,
}

#[phoxal::service(id = "declared-subscribe")]
struct DeclaredSubscribe;

#[phoxal::behavior]
impl DeclaredSubscribe {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let state = ctx.latest(api::topic::client().drive().state()).await?;
        Ok((
            Self,
            Self::Api {
                target: ctx.command_publisher(api::topic::client().drive().target()).await?,
                state,
            },
        ))
    }
}

fn main() {}
