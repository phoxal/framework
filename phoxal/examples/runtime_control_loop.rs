//! A minimal scheduled control-loop participant - the canonical authoring example.
//!
//! It shows the whole authoring surface: an `Api` struct of typed handles, a
//! `#[setup]` that builds them from api-local topics and returns `(Self,
//! Self::Api)`, a `#[step]` that reads the latest input and publishes a
//! version-local body at logical time, a `#[shutdown]`, and the default
//! blocking entrypoint.
//!
//! Run it with `cargo run --example runtime_control_loop` (Ctrl-C to stop).

use phoxal::prelude::*;
use phoxal_api::v1 as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    // Keep-last-1 view of the observed drive state.
    state: Latest<api::drive::State>,
    // Publishes the commanded target.
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "avoid-obstacles")]
struct AvoidObstacles;

#[phoxal::behavior]
impl AvoidObstacles {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                // Api-local topic builders bind each handle's body to its version.
                state: ctx.latest(api::topic::new().drive().state()).await?,
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let now = step.time();

        // Trivial policy: creep forward once we have observed a drive state,
        // otherwise hold still. Real runtimes would fuse perception here.
        let target = match api.state.latest() {
            Some(_) => api::drive::Target {
                linear_x_mps: 0.2,
                angular_z_radps: 0.0,
                curvature_limit_radpm: None,
            },
            None => api::drive::Target {
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
                curvature_limit_radpm: None,
            },
        };

        api.target.publish_at(now, target).await?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api) -> Result<()> {
        // Best-effort: a real participant parks/stops actuators here before the bus
        // closes. Nothing to flush in this example.
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<AvoidObstacles>()
}
