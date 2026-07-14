// Plan #15: a tool with only lifecycle (`#[setup]`/`#[shutdown]`),
// `#[snapshot]`, and raw-bus access is the thin-runner shape and must compile.
// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface, just
// `ctx.raw_bus()` and the raw handle constructors.
use phoxal::prelude::*;
use phoxal::raw::Publisher;
use phoxal_api::v1 as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::tool(id = "tool-thin-runner-lifecycle")]
struct ToolThinRunnerLifecycle {
    publisher: Publisher<api::motion::ManualCommand>,
}

#[phoxal::behavior]
impl ToolThinRunnerLifecycle {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let publisher = Publisher::new(ctx.raw_bus(), &api::topic::new().motion().manual())?;
        Ok((Self { publisher }, ()))
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        Ok(())
    }

    #[snapshot]
    fn snapshot(&self) -> bool {
        let _ = &self.publisher;
        true
    }
}

fn main() {}
