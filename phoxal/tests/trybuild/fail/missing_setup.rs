// A participant impl with no #[setup] (D22).
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: CommandPublisher<api::drive::Target>,
}

#[phoxal::service(id = "no-setup")]
struct NoSetup;

#[phoxal::behavior]
impl NoSetup {
    #[step(hz = 10)]
    async fn step(&mut self, _api: &mut Self::Api, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
