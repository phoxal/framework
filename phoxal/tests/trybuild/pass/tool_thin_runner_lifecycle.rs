// Plan #15: a `Tool` with only lifecycle (`#[setup]`/`#[shutdown]`),
// `#[snapshot]`, and raw-bus access is the thin-runner shape and must compile.
use phoxal::prelude::*;
use phoxal::raw::Publisher;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Tool)]
#[phoxal(
    id = "tool-thin-runner-lifecycle",
    api = y2026_1,
    contracts(publishes(api::motion::ManualCommand))
)]
struct ToolThinRunnerLifecycle {
    publisher: Publisher<api::motion::ManualCommand>,
}

#[phoxal::behavior]
impl ToolThinRunnerLifecycle {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let publisher = Publisher::new(ctx.raw_bus(), &api::topic::new().motion().manual())?;
        Ok(Self { publisher })
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        Ok(())
    }

    #[snapshot]
    fn snapshot(&self) -> bool {
        let _ = &self.publisher;
        true
    }
}

fn main() {}
