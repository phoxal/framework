// #[server] (query serving) is rejected in the first slice.
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "srv", api = y2026_1)]
struct Srv {
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
impl Srv {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }

    #[server]
    async fn handle(&mut self, _req: api::drive::Target) -> Result<()> {
        Ok(())
    }
}

fn main() {}
