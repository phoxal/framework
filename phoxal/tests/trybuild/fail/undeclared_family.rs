// Building a handle for a contract family the runtime never declared is a
// compile error (R: Declares<B> unsatisfied — D44). The struct only declares a
// `drive::State` subscriber, but setup tries to publish `drive::Target`.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "undeclared", api = y2026_1)]
struct Undeclared {
    state: Latest<api::drive::State>,
}

#[phoxal::runtime]
impl Undeclared {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let _undeclared = ctx.publisher(api::topic::new().drive().target()).await?;
        Ok(Self {
            state: ctx.subscribe(api::topic::new().drive().state()).latest().await?,
        })
    }
}

fn main() {}
