// The positive counterpart to `fail/undeclared_subscribe_handle.rs`: declaring
// the `Latest<drive::State>` field on `Api` satisfies `Self::Api:
// DeclaresSubscribe<drive::State>`, so the identical `ctx.latest(...)` call
// compiles.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: Publisher<api::drive::Target>,
    state: Latest<api::drive::State>,
}

#[phoxal::service(id = "declared-subscribe")]
struct DeclaredSubscribe;

#[phoxal::behavior]
impl DeclaredSubscribe {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let state = ctx.latest(api::topic::new().drive().state()).await?;
        Ok((
            Self,
            Self::Api {
                target: ctx.publisher(api::topic::new().drive().target()).await?,
                state,
            },
        ))
    }
}

fn main() {}
