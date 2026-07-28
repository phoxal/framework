use phoxal::prelude::*;

#[phoxal::service(id = "bad-step-frequency")]
struct BadStepFrequency;

impl Participant for BadStepFrequency {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }

    #[phoxal::step(hz = 0)]
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
