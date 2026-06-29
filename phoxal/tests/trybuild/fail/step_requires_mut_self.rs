// #[step] takes `&mut self` (the scheduled control loop — D34).
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "step-no-self", api = y2026_1)]
struct StepNoSelf {}

#[phoxal::runtime]
impl StepNoSelf {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[step(hz = 10)]
    async fn step(_step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
