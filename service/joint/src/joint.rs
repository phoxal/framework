//! `joint` - convert component encoder samples into per-joint kinematic state.
//!
//! A scheduled participant that bridges component-level encoders to joint-level state.
//! At setup it enumerates every joint-targeted encoder capability in the robot
//! model, rejecting any with a non-positive gear ratio or zero counts per
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
use phoxal::model::component::capability::{Capability, StructuralTarget};
use phoxal::model::identity::{CapabilityRef, JointId};
use phoxal::prelude::*;

/// One joint-targeted encoder resolved from the robot model.
struct EncoderBinding {
    joint_id: JointId,
    reference: CapabilityRef,
    direction_sign: i8,
    gear_ratio: f64,
}

impl EncoderBinding {
    /// Every joint-targeted encoder the robot declares, ordered by the
    /// capability reference so two runs over the same robot bind the same way.
    fn resolve(robot: &Robot) -> Result<Vec<Self>> {
        let mut bindings = Vec::new();
        for reference in
            robot.capability_refs(|capability| matches!(capability, Capability::Encoder(_)))
        {
            let (encoder, direction_sign) = robot.require_encoder(&reference)?;
            if !(encoder.gear_ratio.is_finite() && encoder.gear_ratio > 0.0) {
                bail!("capability '{reference}' gear_ratio must be finite and > 0");
            }
            if encoder.counts_per_revolution == 0 {
                bail!("capability '{reference}' counts_per_revolution must be > 0");
            }

            let StructuralTarget::Joint { id } = encoder.target.namespaced(&reference.component_id)
            else {
                continue;
            };
            bindings.push(EncoderBinding {
                joint_id: id,
                reference,
                direction_sign,
                gear_ratio: encoder.gear_ratio,
            });
        }
        Ok(bindings)
    }

    /// Joint CONSUMES encoder samples (the encoder driver owns/publishes them), so
    /// this is the client `Subscribe` side from the public builder.
    fn topic(&self) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::encoder::Sample>> {
        api::topic::client()
            .component(&self.reference.component_id)
            .encoder(&self.reference.capability_id)
            .sample()
    }

    /// `sample` expressed at the joint: encoder radians scaled by this
    /// binding's direction sign over its gear ratio.
    ///
    /// `None` when the encoder published a non-finite reading, which no scaling
    /// can make usable.
    fn joint_state(
        &self,
        sample: &api::component::encoder::Sample,
    ) -> Option<api::joint::JointState> {
        if !(sample.position_rad.is_finite() && sample.velocity_radps.is_finite()) {
            return None;
        }
        let scale = f64::from(self.direction_sign) / self.gear_ratio;
        Some(api::joint::JointState {
            position_rad: sample.position_rad * scale,
            velocity_radps: f64::from(sample.velocity_radps) * scale,
            effort_nm: None,
        })
    }
}

/// One bound encoder: its model binding and the subscriber that carries its
/// samples, so a sample can never be scaled by another binding's gear ratio.
struct BoundEncoder {
    binding: EncoderBinding,
    subscriber: Subscriber<api::component::encoder::Sample>,
}

pub(crate) struct Api {
    encoders: Vec<BoundEncoder>,
    states: BTreeMap<JointId, StatePublisher<api::joint::JointState>>,
}

#[phoxal::service(api = Api)]
pub(crate) struct Joint;

impl Participant for Joint {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let bindings = EncoderBinding::resolve(ctx.robot()?)?;
        let joint_ids = bindings
            .iter()
            .map(|binding| binding.joint_id.clone())
            .collect::<BTreeSet<_>>();

        let mut encoders = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let subscriber = ctx.subscriber(binding.topic()).await?;
            encoders.push(BoundEncoder {
                binding,
                subscriber,
            });
        }

        let mut states = BTreeMap::new();
        for joint_id in joint_ids {
            // Joint OWNS each `joint/{id}` node's state telemetry, so this is
            // the owner builder.
            let publisher = ctx.state_publisher(api::topic::owner().joint(&joint_id).state())?;
            states.insert(joint_id, publisher);
        }

        Ok(((), Api { encoders, states }))
    }

    #[phoxal::step(hz = 50)]
    fn step(&self, api: &Self::Api, step: StepContext, _state: &mut Self::State) -> Result<()> {
        let now = step.now();
        let mut latest_by_joint = BTreeMap::new();

        for encoder in &api.encoders {
            // Only the newest sample of this step matters, and only while it is
            // still fresh: a joint whose encoder has gone silent publishes
            // nothing rather than repeating a stale position.
            let mut latest = None;
            while let Some(received) = encoder.subscriber.try_recv() {
                let produced_at = received.metadata.produced_exactly_at();
                latest = produced_at.map(|at| Timed::new(received.body, at));
            }

            let Some(sample) = latest else {
                continue;
            };
            if !sample.fresh_within(now, api::component::encoder::Sample::STALE_AFTER) {
                continue;
            }
            let state = encoder.binding.joint_state(&sample.body).ok_or_else(|| {
                anyhow::anyhow!(
                    "encoder '{}' published a non-finite sample",
                    encoder.binding.joint_id
                )
            })?;
            latest_by_joint.insert(&encoder.binding.joint_id, state);
        }

        for (joint_id, state) in latest_by_joint {
            if let Some(publisher) = api.states.get(joint_id) {
                publisher.publish(&step.token, state)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api;
    use phoxal::model::RobotBuilder;
    use phoxal::model::identity::JointId;

    use super::EncoderBinding;

    fn binding(direction_sign: i8, gear_ratio: f64) -> EncoderBinding {
        EncoderBinding {
            joint_id: JointId::new("arm_joint"),
            reference: "arm.encoder".parse().expect("a normalized reference"),
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

    /// The joint an encoder reports on is the component-local joint it targets,
    /// namespaced under the instance that mounts it - not the capability's own
    /// id, which is why the two are deliberately named differently here.
    #[test]
    fn robot_config_uses_the_canonical_component_joint_namespace() {
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor
                    .motor("motor", "motor_joint")
                    .encoder("encoder", "motor_joint")
            })
            .component("front_left_drive", "drive_motor")
            .build()
            .expect("a valid robot");

        let bindings = EncoderBinding::resolve(&robot).unwrap();

        let [binding] = bindings.as_slice() else {
            panic!("the robot declares exactly one encoder");
        };
        assert_eq!(binding.reference.component_id, "front_left_drive");
        assert_eq!(binding.joint_id, "front_left_drive__motor_joint");
    }

    #[test]
    fn encoder_radians_are_scaled_to_joint_radians() {
        let state = binding(1, 2.0)
            .joint_state(&api::component::encoder::Sample {
                position_rad: 4.0,
                velocity_radps: 6.0,
            })
            .unwrap();

        assert_close(state.position_rad, 2.0);
        assert_close(state.velocity_radps, 3.0);
        assert!(state.effort_nm.is_none());
    }

    #[test]
    fn direction_sign_flips_position_and_velocity() {
        let state = binding(-1, 1.0)
            .joint_state(&api::component::encoder::Sample {
                position_rad: 1.25,
                velocity_radps: 2.5,
            })
            .unwrap();

        assert_close(state.position_rad, -1.25);
        assert_close(state.velocity_radps, -2.5);
    }

    #[test]
    fn non_finite_encoder_samples_are_rejected() {
        assert!(
            binding(1, 1.0)
                .joint_state(&api::component::encoder::Sample {
                    position_rad: f64::NAN,
                    velocity_radps: 1.0,
                })
                .is_none()
        );
        assert!(
            binding(1, 1.0)
                .joint_state(&api::component::encoder::Sample {
                    position_rad: 1.0,
                    velocity_radps: f32::INFINITY,
                })
                .is_none()
        );
    }
}
