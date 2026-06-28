//! Dynamic per-component handles stored as `Vec` and `BTreeMap` fields, derived
//! from the launched robot manifest.

use std::collections::BTreeMap;

use phoxal::api::y2026_1 as api;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::robot::v1::KinematicConfig;
use phoxal::model::v1::Robot;
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
        let bindings = MotionBindings::from_robot(ctx.robot()?)?;

        let mut motors = Vec::new();
        for binding in &bindings.motors {
            motors.push(ctx.publisher(motor_topic(binding)).await?);
        }

        let mut encoders = BTreeMap::new();
        for binding in &bindings.encoders {
            encoders.insert(
                binding.to_string(),
                ctx.subscribe(encoder_topic(binding)).subscriber().await?,
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

struct MotionBindings {
    motors: Vec<CapabilityRef>,
    encoders: Vec<CapabilityRef>,
}

impl MotionBindings {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let (motors, encoders) = match &robot.manifest.motion.kinematic {
            KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                ..
            } => (
                left_actuators
                    .iter()
                    .chain(right_actuators.iter())
                    .cloned()
                    .collect(),
                left_encoders
                    .iter()
                    .chain(right_encoders.iter())
                    .cloned()
                    .collect(),
            ),
            KinematicConfig::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                ..
            } => (
                vec![
                    front_left_actuator.clone(),
                    front_right_actuator.clone(),
                    rear_left_actuator.clone(),
                    rear_right_actuator.clone(),
                ],
                Vec::new(),
            ),
            KinematicConfig::Ackermann {
                steering_actuator,
                drive_actuator,
                steering_encoder,
                drive_encoder,
                ..
            } => {
                let encoders = steering_encoder
                    .iter()
                    .chain(drive_encoder.iter())
                    .cloned()
                    .collect();
                (
                    vec![steering_actuator.clone(), drive_actuator.clone()],
                    encoders,
                )
            }
            KinematicConfig::Omnidirectional {
                actuators,
                encoders,
            } => (actuators.clone(), encoders.clone()),
        };

        for binding in &motors {
            robot.require_motor(binding)?;
        }
        for binding in &encoders {
            robot.require_encoder(binding)?;
        }

        Ok(Self { motors, encoders })
    }
}

fn motor_topic(
    binding: &CapabilityRef,
) -> phoxal::bus::Topic<phoxal::bus::PubSub<api::component::motor::Command>> {
    api::topic::new()
        .component(&binding.component_id)
        .motor(&binding.capability_id)
        .command()
}

fn encoder_topic(
    binding: &CapabilityRef,
) -> phoxal::bus::Topic<phoxal::bus::PubSub<api::component::encoder::Sample>> {
    api::topic::new()
        .component(&binding.component_id)
        .encoder(&binding.capability_id)
        .sample()
}
