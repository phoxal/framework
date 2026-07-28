use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    initial: u64,
}

struct State(u64);
struct Api;

#[phoxal::service(id = "explicit-types", config = Config, state = State, api = Api)]
struct ExplicitTypes;

impl Participant for ExplicitTypes {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok((State(config.initial), Api))
    }

    #[phoxal::step(hz = 20)]
    async fn step(
        &self,
        _api: &Self::Api,
        _step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        state.0 += 1;
        Ok(())
    }
}

fn main() {}
