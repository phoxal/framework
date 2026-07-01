//! A participant using the advanced async entrypoint.
//!
//! Run with `cargo run --example runtime_async_entrypoint` or inspect metadata
//! with `cargo run --example runtime_async_entrypoint emit-apis`.

use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Service)]
#[phoxal(id = "async-heartbeat", api = y2026_1)]
struct AsyncHeartbeat {
    heartbeat: Publisher<api::presence::Heartbeat>,
}

#[phoxal::behavior]
impl AsyncHeartbeat {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            heartbeat: ctx
                .publisher(api::topic::new().presence().heartbeat())
                .await?,
        })
    }

    #[step(hz = 1)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.heartbeat
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
