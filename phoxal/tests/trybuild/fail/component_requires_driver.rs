// `SetupContextDriverExt::component()` is only available to `#[phoxal::driver]`
// participants. A service is an ordinary participant and cannot read a bound
// `components.instances` entry.
use phoxal::prelude::*;
use phoxal_api::v1 as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    state: Latest<api::drive::State>,
    target: Publisher<api::drive::Target>,
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
                state: ctx.latest(api::topic::new().drive().state()).await?,
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }
}

fn main() {}
