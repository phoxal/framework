// A participant impl with no #[setup] (D22).
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "no-setup", api = y2026_1)]
struct NoSetup {
    target: Publisher<api::drive::Target>,
}

#[phoxal::behavior]
impl NoSetup {
    #[step(hz = 10)]
    async fn step(&mut self, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
