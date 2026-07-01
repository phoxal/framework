// `SetupContext::component()` is only available to `#[derive(phoxal::Driver)]`
// runtimes. A service is an ordinary participant and cannot read a bound
// `components.instances` entry.
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Service)]
#[phoxal(id = "component-requires-driver", api = y2026_1)]
struct ComponentRequiresDriver {
    state: Latest<api::drive::State>,
    target: Publisher<api::drive::Target>,
}

#[phoxal::behavior]
impl ComponentRequiresDriver {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _component = ctx.component()?;
        Ok(Self {
            state: ctx.subscribe(api::topic::new().drive().state()).latest().await?,
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }
}

fn main() {}
