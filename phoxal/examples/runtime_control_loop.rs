//! A minimal scheduled control-loop participant - the canonical authoring example.
//!
//! It shows the whole authoring surface: an `Api` struct of typed handles, a
//! `Participant::setup` that builds them from typed topics, a scheduled
//! `Participant::step`, graceful shutdown, and the default
//! blocking entrypoint.
//!
//! Run it with `cargo run --example runtime_control_loop` (Ctrl-C to stop).

use phoxal::api;
use phoxal::prelude::*;

struct Api {
    // Keep-last-1 view of the observed drive state.
    state: Latest<api::drive::State>,
    // Publishes the commanded target.
    target: CommandPublisher<api::drive::Target>,
}

#[phoxal::service(id = "avoid-obstacles", api = Api)]
struct AvoidObstacles;

impl Participant for AvoidObstacles {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok((
            (),
            Api {
                state: ctx.latest(api::topic::client().drive().state()).await?,
                target: ctx
                    .command_publisher(api::topic::client().drive().target())
                    .await?,
            },
        ))
    }

    #[phoxal::step(hz = 50)]
    async fn step(
        &self,
        api: &Self::Api,
        _step: StepContext,
        _state: &mut Self::State,
    ) -> Result<()> {
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

        api.target.send(target)?;
        Ok(())
    }

    async fn shutdown(
        &self,
        _ctx: ShutdownContext,
        _api: &Self::Api,
        _state: &mut Self::State,
    ) -> Result<()> {
        // Best-effort: a real participant parks/stops actuators here before the bus
        // closes. Nothing to flush in this example.
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<AvoidObstacles>()
}
