// `SetupContextDriverExt::component()` is only available to `#[phoxal::driver]`
// participants. A service is an ordinary participant and cannot read a bound
// `components.instances` entry.
use phoxal::prelude::*;
use phoxal::api as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    state: Latest<api::drive::State>,
    target: CommandPublisher<api::drive::Target>,
}

#[phoxal::service(id = "component-requires-driver")]
struct ComponentRequiresDriver;

#[phoxal::behavior]
impl ComponentRequiresDriver {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let _component = ctx.component()?;
        Ok((
            Self,
            Self::Api {
                state: ctx.latest(api::topic::client().drive().state()).await?,
                target: ctx.command_publisher(api::topic::client().drive().target()).await?,
            },
        ))
    }
}

fn main() {}
