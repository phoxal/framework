// DoD #11 (plan #00 / plan #07): the raw bus is not on the default checked-participant
// surface. `SetupContext::bus()` is `pub(crate)`, so a participant's `#[setup]` cannot
// reach around the typed handle builders to the raw `Bus`. Calling `ctx.bus()` from
// a downstream participant must fail to compile as a privacy error.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "raw-bus")]
struct RawBus;

#[phoxal::behavior]
impl RawBus {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: `bus()` is `pub(crate)` - the raw bus is not on the checked-participant
        // surface (DoD #11).
        let _raw = ctx.bus();
        Ok((
            Self,
            Self::Api {
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }
}

fn main() {}
