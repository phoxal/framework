// #[step] must return `Result<()>` (a malformed return shape is rejected).
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "step-bad-return", api = ())]
struct StepBadReturn;

#[phoxal::behavior]
impl StepBadReturn {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, ()))
    }

    #[step(hz = 10)]
    async fn step(&mut self, _step: StepContext) {}
}

fn main() {}
