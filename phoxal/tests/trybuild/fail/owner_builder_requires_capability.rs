// L2 (plan #00): constructing the OWNER side of a topic requires the runner-minted
// `OwnerCap`. The owner entry is `api::topic::internal::new(cap)`, where `cap`
// comes from `ctx.owner_capability()`. Calling it with NO argument must fail to
// compile - on the documented surface, owning a topic cannot happen by accident.
//
// This struct declares the publish handle, so `R: DeclaresPublish<drive::State>`
// is satisfied; the SOLE error is the missing `OwnerCap` argument on the owner
// builder entry.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "owner-needs-cap", api = y2026_1)]
struct OwnerNeedsCap {
    state: Publisher<api::drive::State>,
}

#[phoxal::behavior]
impl OwnerNeedsCap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // ERROR: `internal::new` takes the runner-minted `OwnerCap`; this call
        // passes no argument.
        let state = ctx
            .publisher(api::topic::internal::new().drive().state())
            .await?;
        Ok(Self { state })
    }
}

fn main() {}
