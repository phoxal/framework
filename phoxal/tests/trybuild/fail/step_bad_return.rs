// #[step] must return `Result<()>` (a malformed return shape is rejected).
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "step-bad-return", api = y2026_1)]
struct StepBadReturn {}

#[phoxal::runtime]
impl StepBadReturn {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[step(hz = 10)]
    async fn step(&mut self, _step: StepContext) {}
}

fn main() {}
