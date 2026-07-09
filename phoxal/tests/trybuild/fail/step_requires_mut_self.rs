// #[step] takes `&mut self` (the scheduled control loop - D34).
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "step-no-self", api = ())]
struct StepNoSelf;

#[phoxal::behavior]
impl StepNoSelf {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[step(hz = 10)]
    async fn step(_step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
