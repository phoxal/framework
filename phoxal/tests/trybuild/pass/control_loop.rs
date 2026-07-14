// A minimal valid participant: Config + Api + participant impl + setup + step + shutdown.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    state: Latest<api::drive::State>,
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "demo")]
struct Demo;

#[phoxal::behavior]
impl Demo {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                state: ctx.latest(api::topic::new().drive().state()).await?,
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let _ = (step.time(), &api.state);
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api) -> Result<()> {
        Ok(())
    }
}

fn main() {}
