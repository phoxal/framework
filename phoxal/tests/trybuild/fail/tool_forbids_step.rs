// Plan #15: a `Tool` is a thin raw-bus runner (lifecycle + `sd_notify` +
// `participant_id` + `ctx.robot()` + `phoxal::raw`), not a typed-graph
// participant, so `#[step]` is not allowed on it.
use phoxal::prelude::*;

#[derive(phoxal::Tool)]
#[phoxal(id = "tool-forbids-step", api = y2026_1)]
struct ToolForbidsStep {}

#[phoxal::behavior]
impl ToolForbidsStep {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[step(hz = 10)]
    async fn step(&mut self, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
