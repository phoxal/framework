//! Dynamic per-component handles stored as `Vec` and `BTreeMap` fields.
//!
//! This mirrors the shape official drivers use after resolving component
//! bindings from a robot model.

use std::collections::BTreeMap;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "dynamic-handles", api = y2026_1)]
struct DynamicHandles {
    motors: Vec<Publisher<api::component::motor::Command>>,
    encoders: BTreeMap<String, Subscriber<api::component::encoder::Sample>>,
}

#[phoxal::runtime]
impl DynamicHandles {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let mut motors = Vec::new();
        for (instance, capability) in [("base", "left"), ("base", "right")] {
            motors.push(
                ctx.publisher(
                    api::topic::new()
                        .component(instance)
                        .motor(capability)
                        .command(),
                )
                .await?,
            );
        }

        let mut encoders = BTreeMap::new();
        for (instance, capability) in [("base", "left_encoder"), ("base", "right_encoder")] {
            encoders.insert(
                capability.to_string(),
                ctx.subscribe(
                    api::topic::new()
                        .component(instance)
                        .encoder(capability)
                        .sample(),
                )
                .subscriber()
                .await?,
            );
        }

        Ok(Self { motors, encoders })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        for encoder in self.encoders.values() {
            while let Some(_sample) = encoder.try_recv() {}
        }
        for motor in &self.motors {
            motor
                .publish_at(step.time(), api::component::motor::Command::Stop)
                .await?;
        }
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<DynamicHandles>()
}
