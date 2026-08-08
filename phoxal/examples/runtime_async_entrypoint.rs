//! A participant using the advanced async entrypoint.
//!
//! Run with `cargo run --example runtime_async_entrypoint`.

use phoxal::prelude::*;

#[phoxal::service(id = "async-participant")]
struct AsyncParticipant;

impl Participant for AsyncParticipant {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }

    #[phoxal::step(hz = 1)]
    fn step(&self, _api: &Self::Api, _step: StepContext, _state: &mut Self::State) -> Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> phoxal::Result<()> {
    phoxal::tokio::run::<AsyncParticipant>().await
}
