// A minimal valid participant: derive + participant impl + setup + step + shutdown.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "demo", api = y2026_1)]
struct Demo {
    state: Latest<api::drive::State>,
    target: Publisher<api::drive::Target>,
}

#[phoxal::behavior]
impl Demo {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            state: ctx.subscribe(api::topic::new().drive().state()).latest().await?,
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let _ = step.time();
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}

fn main() {}
