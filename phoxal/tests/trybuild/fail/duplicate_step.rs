// Two #[step] methods — a runtime has at most one scheduled loop.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "dup-step", api = y2026_1)]
struct DupStep {
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
impl DupStep {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }

    #[step(hz = 10)]
    async fn step_a(&mut self, _step: StepContext) -> Result<()> {
        Ok(())
    }

    #[step(hz = 20)]
    async fn step_b(&mut self, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
