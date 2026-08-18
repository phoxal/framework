use phoxal::api;
use phoxal::identity::ComponentInstanceId;
use phoxal::model::identity::CapabilityId;
use phoxal::prelude::*;

struct Api {
    ranges: Vec<SamplePublisher<api::component::range::Sample>>,
}

#[phoxal::driver(id = "dynamic-handles", api = Api)]
struct DynamicHandles;

impl Participant for DynamicHandles {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let chassis = ComponentInstanceId::new("chassis")?;
        let mut ranges = Vec::new();
        for capability in ["front", "rear"] {
            let capability = CapabilityId::new(capability)?;
            ranges.push(
                ctx.sample_publisher(
                    api::topics()
                        .component(&chassis)?
                        .range(&capability)?
                        .sample()
                        .owner(),
                )?,
            );
        }
        Ok(((), Api { ranges }))
    }
}

fn main() {}
