//! `joint` - convert component encoder samples into per-joint kinematic state.
//!
//! A scheduled participant that bridges component-level encoders to joint-level state.
//! At setup it enumerates every joint-targeted encoder capability in the robot
//! model (D33), rejecting any with a non-positive gear ratio or zero counts per
//! revolution. A robot with no joint-targeted encoder reports `Inactive`.
//! It subscribes to the per-capability `component/<id>/encoder/<cap>/sample` topic
//! for each binding, and publishes the per-joint `joint/<id>/state` topic.
//! Each step it takes the latest encoder sample per joint, scales position and
//! velocity by the binding's direction sign over its gear ratio, and publishes the
//! resulting joint state; effort is left unset (no torque is estimated).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::component::v0::capability::{Capability, StructuralTarget};
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

const ENCODER_STALE_NS: u64 = 200_000_000;

#[derive(Clone, Debug)]
struct EncoderBinding {
    joint_id: String,
    component_id: String,
    capability_id: String,
    direction_sign: i8,
    gear_ratio: f64,
}

impl EncoderBinding {
    /// Joint CONSUMES encoder samples (the encoder driver owns/publishes them), so
    /// this is the client `Subscribe` side from the public builder.
    fn topic(&self) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::encoder::Sample>> {
        api::topic::new()
            .component(&self.component_id)
            .encoder(&self.capability_id)
            .sample()
    }
}

struct JointConfig {
    encoders: Vec<EncoderBinding>,
}

#[derive(phoxal::Api)]
struct Api {
    encoders: Vec<Subscriber<api::component::encoder::Sample>>,
    states: BTreeMap<String, Publisher<api::joint::JointState>>,
}

impl JointConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let mut encoders = Vec::new();

        for component_id in robot.manifest.components().keys() {
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

        Ok(Self { encoders })
    }
}

#[phoxal::service(id = "joint", config = ())]
struct Joint {
    config: JointConfig,
    sample_at: Vec<Option<LogicalTime>>,
}

#[phoxal::behavior]
impl Joint {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let config = JointConfig::from_robot(ctx.robot()?)?;

        let mut encoders = Vec::with_capacity(config.encoders.len());
        for binding in &config.encoders {
            encoders.push(ctx.subscriber(binding.topic(), 32).await?);
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
                // Joint OWNS each `joint/{id}` node's state telemetry -> owner
                // (`internal`) builder.
                ctx.publisher(api::topic::internal::new(cap).joint(&joint_id).state())
                    .await?,
            );
        }

        Ok((
            Self {
                sample_at: vec![None; config.encoders.len()],
                config,
            },
            Self::Api { encoders, states },
        ))
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let mut latest_by_joint = BTreeMap::new();
        let now = step.time();
        for ((subscriber, binding), sample_at) in api
            .encoders
            .iter_mut()
            .zip(&self.config.encoders)
            .zip(&mut self.sample_at)
        {
            let mut latest = None;
            while let Some(received) = subscriber.try_recv() {
                *sample_at = Some(LogicalTime::new(
                    received.metadata.epoch,
                    received.metadata.produced_at_ns,
                ));
                latest = Some(received.body);
            }

            if let Some(sample) = latest
                && sample_at.is_some_and(|at| {
                    at.epoch() == now.epoch()
                        && at.time_ns() <= now.time_ns()
                        && now.time_ns().saturating_sub(at.time_ns()) <= ENCODER_STALE_NS
                })
            {
                let state = joint_state(&sample, binding).ok_or_else(|| {
                    anyhow::anyhow!(
                        "encoder '{}' published a non-finite sample",
                        binding.joint_id
                    )
                })?;
                latest_by_joint.insert(binding.joint_id.clone(), state);
            }
        }

        for (joint_id, state) in latest_by_joint {
            if let Some(publisher) = api.states.get(&joint_id) {
                publisher.publish_at(step.time(), state).await?;
            }
        }
        Ok(())
    }
}

fn joint_state(
    sample: &api::component::encoder::Sample,
    binding: &EncoderBinding,
) -> Option<api::joint::JointState> {
    if !(sample.position_rad.is_finite() && sample.velocity_radps.is_finite()) {
        return None;
    }
    let scale = f64::from(binding.direction_sign) / binding.gear_ratio;
    Some(api::joint::JointState {
        position_rad: sample.position_rad * scale,
        velocity_radps: f64::from(sample.velocity_radps) * scale,
        effort_nm: None,
    })
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Joint>()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

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
            &api::component::encoder::Sample {
                position_rad: 4.0,
                velocity_radps: 6.0,
            },
            &binding(1, 2.0),
        )
        .unwrap();

        assert_close(state.position_rad, 2.0);
        assert_close(state.velocity_radps, 3.0);
        assert!(state.effort_nm.is_none());
    }

    #[test]
    fn direction_sign_flips_position_and_velocity() {
        let state = joint_state(
            &api::component::encoder::Sample {
                position_rad: 1.25,
                velocity_radps: 2.5,
            },
            &binding(-1, 1.0),
        )
        .unwrap();

        assert_close(state.position_rad, -1.25);
        assert_close(state.velocity_radps, -2.5);
    }

    #[test]
    fn non_finite_encoder_samples_are_rejected() {
        assert!(
            joint_state(
                &api::component::encoder::Sample {
                    position_rad: f64::NAN,
                    velocity_radps: 1.0,
                },
                &binding(1, 1.0),
            )
            .is_none()
        );
        assert!(
            joint_state(
                &api::component::encoder::Sample {
                    position_rad: 1.0,
                    velocity_radps: f32::INFINITY,
                },
                &binding(1, 1.0),
            )
            .is_none()
        );
    }

    #[test]
    fn config_from_robot_enumerates_joint_targeted_encoders() {
        let robot = phoxal::model::v0::Robot::read_from_dir(fixture()).unwrap();
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
    fn no_joint_encoders_is_a_valid_inactive_configuration() {
        let robot = phoxal::model::v0::Robot::read_from_dir(fixture()).unwrap();
        let mut robot = robot;
        robot.components.values_mut().for_each(|component| {
            component.capabilities.retain(|_, capability| {
                !matches!(
                    capability,
                    phoxal::model::component::v0::capability::Capability::Encoder(_)
                )
            });
        });
        assert!(JointConfig::from_robot(&robot).unwrap().encoders.is_empty());
    }

    #[test]
    fn api_reports_contracts() {
        assert_eq!(<Joint as Participant>::ID, "joint");

        let contracts = <<Joint as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::component::encoder::Sample as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::joint::JointState as ContractBody>::TOPIC
                && c.role == ContractRole::Publish
        }));
    }
}
