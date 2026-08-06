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
use phoxal::model::Robot;
use phoxal::model::component::CapabilityRef;
use phoxal::model::component::capability::{Capability, StructuralTarget};
use phoxal::prelude::*;

const ENCODER_STALE: std::time::Duration = std::time::Duration::from_millis(200);

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
        api::topic::client()
            .component(&self.component_id)
            .encoder(&self.capability_id)
            .sample()
    }
}

struct JointConfig {
    encoders: Vec<EncoderBinding>,
}

pub struct Api {
    encoders: Vec<Subscriber<api::component::encoder::Sample>>,
    states: BTreeMap<String, StatePublisher<api::joint::JointState>>,
}

impl JointConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let mut encoders = Vec::new();

        for component_id in robot.component_ids() {
            let component = robot.component_for_instance(component_id)?;
            for (capability_id, capability) in component.capabilities() {
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
                    component_id: component_id.to_string(),
                    capability_id: capability_id.to_string(),
                    direction_sign,
                    gear_ratio: encoder.gear_ratio,
                });
            }
        }

        Ok(Self { encoders })
    }
}

pub struct JointState {
    config: JointConfig,
    sample_at: Vec<Option<RobotInstant>>,
}

#[phoxal::service(state = JointState, api = Api)]
pub struct Joint;

impl Participant for Joint {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
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
                // owner builder.
                ctx.state_publisher(api::topic::owner().joint(&joint_id).state())
                    .await?,
            );
        }

        Ok((
            JointState {
                sample_at: vec![None; config.encoders.len()],
                config,
            },
            Api { encoders, states },
        ))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        state.sample_at.fill(None);
        Ok(())
    }

    #[phoxal::step(hz = 50)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        let mut latest_by_joint = BTreeMap::new();
        let now = step.now();
        for ((subscriber, binding), sample_at) in api
            .encoders
            .iter()
            .zip(&state.config.encoders)
            .zip(&mut state.sample_at)
        {
            let mut latest = None;
            while let Some(received) = subscriber.try_recv() {
                *sample_at = received.metadata.produced_exactly_at();
                latest = Some(received.body);
            }

            if let Some(sample) = latest
                && sample_at.is_some_and(|at| {
                    TimeWindow::exact(at)
                        .possibly_fresh_within(now, ENCODER_STALE)
                        .unwrap_or(false)
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
                publisher.publish(step.token(), state)?;
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

#[cfg(test)]
mod tests {
    use phoxal::api;
    use phoxal::model::Robot;

    use super::{EncoderBinding, JointConfig, joint_state};

    /// Stage a finalized bundle from the checked-in authored fixtures and take
    /// its canonical model.
    ///
    /// Finalization belongs to `phoxal-cli`; the two edits it makes to the
    /// authored document (pin the resolved clock, resolve the structure path
    /// into `assets/`) are small enough to reproduce here, so this crate reads
    /// a real bundle without any bundle being checked in.
    fn fixture() -> Robot {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixture");
        let project = fixture.join("robot/rgbd-imu-diff-drive");
        let bundle = tempfile::tempdir().expect("a staging directory");
        let assets = bundle.path().join("assets");

        std::fs::create_dir_all(assets.join("robot")).unwrap();
        std::fs::copy(
            project.join("structure.urdf"),
            assets.join("robot/structure.urdf"),
        )
        .unwrap();
        for component_type in ["camera_rgbd_640x480", "drive_motor", "imu", "range_tof"] {
            let source = fixture.join("component").join(component_type);
            let staged = assets.join("components").join(component_type);
            std::fs::create_dir_all(&staged).unwrap();
            for document in ["component.yaml", "simulation.yaml", "structure.urdf"] {
                std::fs::copy(source.join(document), staged.join(document)).unwrap();
            }
            for mesh in std::fs::read_dir(source.join("meshes"))
                .into_iter()
                .flatten()
            {
                let mesh = mesh.unwrap();
                std::fs::create_dir_all(staged.join("meshes")).unwrap();
                std::fs::copy(mesh.path(), staged.join("meshes").join(mesh.file_name())).unwrap();
            }
        }
        std::fs::write(
            bundle.path().join("robot.yaml"),
            std::fs::read_to_string(project.join("robot.yaml"))
                .unwrap()
                .replacen(
                    "schema: phoxal/robot/v0",
                    "schema: phoxal/robot/v0\nclock: real",
                    1,
                )
                .replacen(
                    "structure: structure.urdf",
                    "structure: robot/structure.urdf",
                    1,
                ),
        )
        .unwrap();
        phoxal::bundle::FinalizedBundle::load(bundle.path())
            .expect("the staged bundle must load")
            .into_robot()
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
    fn robot_config_uses_the_canonical_component_joint_namespace() {
        let robot = fixture();
        let config = JointConfig::from_robot(&robot).unwrap();
        assert!(config.encoders.iter().any(|binding| {
            binding.component_id == "front_left_drive"
                && binding.joint_id == "front_left_drive__motor_joint"
        }));
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
}
