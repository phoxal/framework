// L2 (plan #00): constructing the OWNER side of a topic requires the runner-minted
// `OwnerCap`. The owner entry is `api::topic::internal::new(cap)`, where `cap`
// comes from `ctx.owner_capability()`. Calling it with NO argument must fail to
// compile - on the documented surface, owning a topic cannot happen by accident.
//
// The SOLE error is the missing `OwnerCap` argument on the owner builder entry.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    state: Publisher<api::drive::State>,
}

#[phoxal::service(id = "owner-needs-cap")]
struct OwnerNeedsCap;

#[phoxal::behavior]
impl OwnerNeedsCap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: `internal::new` takes the runner-minted `OwnerCap`; this call
        // passes no argument.
        let state = ctx
            .publisher(api::topic::internal::new().drive().state())
            .await?;
        Ok((Self, Self::Api { state }))
    }
}

fn main() {}
