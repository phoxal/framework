//! A minimal scheduled control-loop runtime — the canonical authoring example.
//!
//! It shows the whole first-slice surface: one struct of typed handles, a
//! `#[setup]` that builds them from api-local topics, a `#[step]` that reads the
//! latest input and publishes a version-local body at logical time, a
//! `#[shutdown]`, and the default blocking entrypoint.
//!
//! Run it with `cargo run --example runtime_control_loop` (Ctrl-C to stop) or
//! inspect its metadata with `cargo run --example runtime_control_loop emit-apis`.

// Author against exactly one dated API version (D60).
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Service)]
#[phoxal(id = "avoid-obstacles", api = y2026_1)]
struct AvoidObstacles {
    // Keep-last-1 view of the observed drive state.
    state: Latest<api::drive::State>,
    // Publishes the commanded target.
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
impl AvoidObstacles {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            // Api-local topic builders bind each handle's body to this API version.
            state: ctx
                .subscribe(api::topic::new().drive().state())
                .latest()
                .await?,
            target: ctx.publisher(api::topic::new().drive().target()).await?,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let now = step.time();

        // Trivial policy: creep forward once we have observed a drive state,
        // otherwise hold still. Real runtimes would fuse perception here.
        let target = match self.state.latest() {
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

        self.target.publish_at(now, target).await?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self) -> Result<()> {
        // Best-effort: a real runtime parks/stops actuators here before the bus
        // closes. Nothing to flush in this example.
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<AvoidObstacles>()
}
