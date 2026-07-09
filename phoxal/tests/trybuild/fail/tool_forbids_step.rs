// Plan #15: a tool is a thin raw-bus runner (lifecycle + `sd_notify` +
// `participant_id` + `ctx.robot()` + `phoxal::raw`), not a typed-graph
// participant, so `#[step]` is not allowed on it.
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::tool(id = "tool-forbids-step")]
struct ToolForbidsStep;

#[phoxal::behavior]
impl ToolForbidsStep {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[step(hz = 10)]
    async fn step(&mut self, _api: &mut Self::Api, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
