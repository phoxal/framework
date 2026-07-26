// Two #[step] methods - a participant has at most one scheduled loop.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: CommandPublisher<api::drive::Target>,
}

#[phoxal::service(id = "dup-step")]
struct DupStep;

#[phoxal::behavior]
impl DupStep {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                target: ctx.command_publisher(api::topic::client().drive().target()).await?,
            },
        ))
    }

    #[step(hz = 10)]
    async fn step_a(&mut self, _step: StepContext) -> Result<()> {
        Ok(())
    }

    #[step(hz = 20)]
    async fn step_b(&mut self, _step: StepContext) -> Result<()> {
        Ok(())
    }
}

fn main() {}
