//! A participant using the advanced async entrypoint.
//!
//! Run with `cargo run --example runtime_async_entrypoint`.

use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {}

#[phoxal::service(id = "async-participant")]
struct AsyncParticipant;

#[phoxal::behavior]
impl AsyncParticipant {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((Self, Self::Api {}))
    }

    #[step(hz = 1)]
    async fn step(&mut self, _api: &mut Self::Api, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> phoxal::Result<()> {
    phoxal::tokio::run::<AsyncParticipant>().await
}
