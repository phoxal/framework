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
    state: StateView<api::drive::State>,
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
                state: ctx.state_view(api::topic::client().drive().state()).await?,
                target: ctx.command_publisher(api::topic::client().drive().target())?,
            },
        ))
    }

    #[phoxal::step(hz = 50)]
    fn step(&self, api: &Self::Api, _step: StepContext, _state: &mut Self::State) -> Result<()> {
        // Trivial policy: creep forward once we have observed a drive state,
        // otherwise hold still. Real runtimes would fuse perception here.
        let target = match api.state.latest() {
            Some(_) => api::drive::Target::try_new(0.2, 0.0)?,
            None => api::drive::Target::stopped(),
        };

        api.target.send(target)?;
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<AvoidObstacles>()
}
