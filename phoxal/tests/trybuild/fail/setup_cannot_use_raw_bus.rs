// DoD #11 (plan #00 / plan #07): the raw bus is not on the default checked-runtime
// surface. `SetupContext::bus()` is `pub(crate)`, so a runtime's `#[setup]` cannot
// reach around the typed handle builders to the raw `Bus`. Calling `ctx.bus()` from
// a downstream runtime must fail to compile as a privacy error.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "raw-bus", api = y2026_1)]
struct RawBus {
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
impl RawBus {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // ERROR: `bus()` is `pub(crate)` - the raw bus is not on the checked-runtime
        // surface (DoD #11).
        let _raw = ctx.bus();
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }
}

fn main() {}
