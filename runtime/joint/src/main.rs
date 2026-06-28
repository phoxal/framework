//! `joint` - convert component encoder samples into per-joint kinematic state.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use phoxal::api::y2026_1 as api;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::{Capability, StructuralTarget};
use phoxal::model::v1::Robot;
use phoxal::prelude::*;

#[derive(Clone, Debug)]
struct EncoderBinding {
    joint_id: String,
    component_id: String,
    capability_id: String,
    direction_sign: i8,
    gear_ratio: f64,
}

impl EncoderBinding {
    fn topic(&self) -> phoxal::bus::Topic<phoxal::bus::PubSub<api::component::EncoderSample>> {
        api::topic::new()
            .component()
            .encoder_sample(&self.component_id, &self.capability_id)
    }
}

struct JointConfig {
    encoders: Vec<EncoderBinding>,
}

impl JointConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let mut encoders = Vec::new();

        for component_id in robot.manifest.components.keys() {
            let component = robot.component_for_instance(component_id)?;
            for (capability_id, capability) in &component.capabilities {
                let Capability::Encoder(_) = capability else {
                    continue;
                };

                let reference = CapabilityRef::new(component_id, capability_id);
                let (encoder, direction_sign) = robot.require_encoder(&reference)?;
                if !(encoder.gear_ratio.is_finite() && encoder.gear_ratio > 0.0) {
                    bail!("capability '{reference}' gear_ratio must be finite and > 0");
                }
                if encoder.counts_per_revolution == 0 {
                    bail!("capability '{reference}' counts_per_revolution must be > 0");
                }

                let target = encoder.target.namespaced(component_id);
                let StructuralTarget::Joint { id } = target else {
                    continue;
                };

                encoders.push(EncoderBinding {
                    joint_id: id,
                    component_id: component_id.clone(),
                    capability_id: capability_id.clone(),
                    direction_sign,
                    gear_ratio: encoder.gear_ratio,
                });
            }
        }

        if encoders.is_empty() {
            bail!("joint runtime requires at least one joint-targeted encoder capability");
        }

        Ok(Self { encoders })
    }
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "joint", api = y2026_1)]
struct Joint {
    config: JointConfig,
    encoders: Vec<Subscriber<api::component::EncoderSample>>,
    states: BTreeMap<String, Publisher<api::joint::JointState>>,
}

#[phoxal::runtime]
impl Joint {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let config = JointConfig::from_robot(ctx.robot()?)?;

        let mut encoders = Vec::with_capacity(config.encoders.len());
        for binding in &config.encoders {
            encoders.push(ctx.subscribe(binding.topic()).subscriber().await?);
        }

        let joint_ids = config
            .encoders
            .iter()
            .map(|binding| binding.joint_id.clone())
            .collect::<BTreeSet<_>>();
        let mut states = BTreeMap::new();
        for joint_id in joint_ids {
            states.insert(
                joint_id.clone(),
                ctx.publisher(api::topic::new().joint().state(&joint_id))
                    .await?,
            );
        }

        Ok(Self {
            config,
            encoders,
            states,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let mut latest_by_joint = BTreeMap::new();
        for (subscriber, binding) in self.encoders.iter_mut().zip(&self.config.encoders) {
            let mut latest = None;
            while let Some(received) = subscriber.try_recv() {
                latest = Some(received.body);
            }

            if let Some(sample) = latest {
                latest_by_joint.insert(binding.joint_id.clone(), joint_state(&sample, binding));
            }
        }

        for (joint_id, state) in latest_by_joint {
            if let Some(publisher) = self.states.get(&joint_id) {
                publisher.publish_at(step.time(), state).await?;
            }
        }
        Ok(())
    }
}

fn joint_state(
    sample: &api::component::EncoderSample,
    binding: &EncoderBinding,
) -> api::joint::JointState {
    let scale = f64::from(binding.direction_sign) / binding.gear_ratio;
    api::joint::JointState {
        position_rad: sample.position_rad * scale,
        velocity_radps: f64::from(sample.velocity_radps) * scale,
        effort_nm: None,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Joint>()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

    use super::{EncoderBinding, Joint, JointConfig, joint_state};

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixture/robot/rgbd-imu-diff-drive")
    }

    fn binding(direction_sign: i8, gear_ratio: f64) -> EncoderBinding {
        EncoderBinding {
            joint_id: "arm_joint".to_string(),
            component_id: "arm".to_string(),
            capability_id: "encoder".to_string(),
            direction_sign,
            gear_ratio,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn encoder_radians_are_scaled_to_joint_radians() {
        let state = joint_state(
            &api::component::EncoderSample {
                position_rad: 4.0,
                velocity_radps: 6.0,
            },
            &binding(1, 2.0),
        );

        assert_close(state.position_rad, 2.0);
        assert_close(state.velocity_radps, 3.0);
        assert!(state.effort_nm.is_none());
    }

    #[test]
    fn direction_sign_flips_position_and_velocity() {
        let state = joint_state(
            &api::component::EncoderSample {
                position_rad: 1.25,
                velocity_radps: 2.5,
            },
            &binding(-1, 1.0),
        );

        assert_close(state.position_rad, -1.25);
        assert_close(state.velocity_radps, -2.5);
    }

    #[test]
    fn config_from_robot_enumerates_joint_targeted_encoders() {
        let robot = phoxal::model::v1::Robot::read_from_dir(fixture()).unwrap();
        let config = JointConfig::from_robot(&robot).unwrap();

        assert_eq!(config.encoders.len(), 4);
        assert!(
            config
                .encoders
                .iter()
                .all(|binding| binding.joint_id.ends_with("__motor_joint"))
        );
        assert!(
            config
                .encoders
                .iter()
                .any(|binding| binding.direction_sign == -1)
        );
    }

    #[test]
    fn emit_apis_reports_contracts() {
        let metadata = phoxal::runtime::runtime_metadata::<Joint>();
        assert_eq!(metadata.artifact.id, "joint");

        let contracts = metadata.required_contracts;
        assert!(contracts.iter().any(|c| {
            c.family == <api::component::EncoderSample as ContractBody>::FAMILY
                && c.direction == phoxal::runtime::Direction::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.family == <api::joint::JointState as ContractBody>::FAMILY
                && c.direction == phoxal::runtime::Direction::Publish
        }));
    }
}
