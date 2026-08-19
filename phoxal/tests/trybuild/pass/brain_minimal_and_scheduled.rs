// The two shapes a root brain takes: the minimal no-op composition root, and a
// brain with typed state, a typed api, and a scheduled step. Both use the
// ordinary checked participant surface - there is no brain-specific runner.
use phoxal::api;
use phoxal::prelude::*;

#[phoxal::brain]
struct MinimalBrain;

impl Participant for MinimalBrain {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

#[derive(Default)]
struct MissionState {
    steps: u64,
}

struct MissionApi {
    drive: StateView<api::drive::State>,
    target: SetpointPublisher<api::drive::Target>,
}

#[phoxal::brain(state = MissionState, api = MissionApi)]
struct MissionBrain;

impl Participant for MissionBrain {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok((
            MissionState::default(),
            MissionApi {
                drive: ctx.state_view(api::topics().drive().state().client()).await?,
                target: ctx
                    .setpoint_publisher(api::topics().drive().target().client())?,
            },
        ))
    }

    #[phoxal::step(hz = 10)]
    fn step(
        &self,
        api: &Self::Api,
        _step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        state.steps += 1;
        let _ = api.drive.latest();
        api.target.send(api::drive::Target::stopped())?;
        Ok(())
    }
}

fn main() {
    fn unit_config<T: Participant<Config = ()>>() {}
    unit_config::<MinimalBrain>();
    unit_config::<MissionBrain>();
}
