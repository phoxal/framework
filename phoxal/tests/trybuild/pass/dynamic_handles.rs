use phoxal::api;
use phoxal::prelude::*;

struct Api {
    ranges: Vec<MeasurementPublisher<api::component::range::Sample>>,
}

#[phoxal::driver(id = "dynamic-handles", api = Api)]
struct DynamicHandles;

impl Participant for DynamicHandles {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let mut ranges = Vec::new();
        for capability in ["front", "rear"] {
            ranges.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component("chassis")?
                        .range(capability)?
                        .sample(),
                )?,
            );
        }
        Ok(((), Api { ranges }))
    }
}

fn main() {}
