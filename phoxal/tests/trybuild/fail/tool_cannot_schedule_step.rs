use phoxal::prelude::*;

#[phoxal::tool(id = "scheduled-tool")]
struct ScheduledTool;

impl Participant for ScheduledTool {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }

    #[phoxal::step(hz = 10)]
    async fn step(
        &self,
        _api: &Self::Api,
        _step: StepContext,
        _state: &mut Self::State,
    ) -> Result<()> {
        Ok(())
    }
}

fn main() {}
