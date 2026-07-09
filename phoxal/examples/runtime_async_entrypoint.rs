//! A participant using the advanced async entrypoint.
//!
//! Run with `cargo run --example runtime_async_entrypoint`.

use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    heartbeat: Publisher<api::presence::Heartbeat>,
}

#[phoxal::service(id = "async-heartbeat")]
struct AsyncHeartbeat;

#[phoxal::behavior]
impl AsyncHeartbeat {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                heartbeat: ctx
                    .publisher(api::topic::new().presence().heartbeat())
                    .await?,
            },
        ))
    }

    #[step(hz = 1)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        api.heartbeat
            .publish_at(
                step.time(),
                api::presence::Heartbeat {
                    participant: "async-heartbeat".to_string(),
                    readiness: api::presence::Readiness::Ready,
                },
            )
            .await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> phoxal::Result<()> {
    phoxal::tokio::run::<AsyncHeartbeat>().await
}
