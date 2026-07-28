use phoxal::prelude::*;

#[phoxal::simulator(id = "scheduled-simulator")]
struct ScheduledSimulator;

impl Participant for ScheduledSimulator {
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
