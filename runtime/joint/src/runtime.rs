use std::collections::BTreeMap;
use std::f64::consts::TAU;
use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api::component::v1::capability::encoder::{self, Sample as EncoderSample};
use phoxal::api::joint::v1::{JointId, JointState, Quantity, data};
use phoxal::bus::pubsub::Stamped;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::{Capability, StructuralTarget};
use phoxal::runtime::clock::Step;
use phoxal::runtime::staged::Robot;
use phoxal::runtime::step::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use tracing::warn;

#[derive(Clone)]
pub struct Config {
    encoders: Vec<JointEncoder>,
    clock_period: Duration,
}

impl Config {
    pub fn from_robot(robot: &Robot) -> Result<Self> {
        let mut encoders = Vec::new();

        for component_id in robot.model.components.keys() {
            let component = robot.component_for_instance(component_id)?;
            for (capability_id, capability) in &component.capabilities {
                let Capability::Encoder(_) = capability else {
                    continue;
                };

                let target = capability.target().namespaced(component_id);
                let StructuralTarget::Joint { id } = target else {
                    continue;
                };

                let reference = CapabilityRef::new(component_id, capability_id);
                let resolved = robot.require_encoder(&reference)?;
                encoders.push(JointEncoder::new(
                    JointId::new(id),
                    resolved.reference,
                    resolved.direction_sign,
                    resolved.gear_ratio,
                    resolved.counts_per_revolution,
                    resolved.encoder.publish_rate_hz,
                )?);
            }
        }

        if encoders.is_empty() {
            bail!("joint runtime requires at least one joint-targeted encoder capability");
        }

        let publish_rate_hz = encoders
            .iter()
            .map(|encoder| encoder.publish_rate_hz)
            .fold(0.0, f64::max);

        Ok(Self {
            encoders,
            clock_period: Duration::from_secs_f64(1.0 / publish_rate_hz),
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

#[derive(Debug, Clone)]
struct JointEncoder {
    joint_id: JointId,
    reference: CapabilityRef,
    direction_sign: i8,
    gear_ratio: f64,
    counts_per_revolution: u32,
    publish_rate_hz: f64,
}

impl JointEncoder {
    fn new(
        joint_id: JointId,
        reference: CapabilityRef,
        direction_sign: i8,
        gear_ratio: f64,
        counts_per_revolution: u32,
        publish_rate_hz: f64,
    ) -> Result<Self> {
        if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
            bail!("capability '{}' publish_rate_hz must be > 0", reference);
        }

        Ok(Self {
            joint_id,
            reference,
            direction_sign,
            gear_ratio,
            counts_per_revolution,
            publish_rate_hz,
        })
    }
}

pub enum Input {
    Encoder {
        joint_id: JointId,
        sample: Stamped<EncoderSample>,
    },
}

pub struct JointRuntime {
    encoders: BTreeMap<JointId, JointEncoder>,
    publishers: BTreeMap<JointId, Publisher<Stamped<JointState>>>,
}

#[async_trait::async_trait]
impl Runtime for JointRuntime {
    const RUNTIME_ID: &'static str = "joint";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_robot(&common.robot()?)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        let mut publishers = BTreeMap::new();

        for encoder in &config.encoders {
            io.subscribe::<Stamped<EncoderSample>, _>(
                &encoder::topic(
                    &encoder.reference.component_id,
                    &encoder.reference.capability_id,
                ),
                {
                    let joint_id = encoder.joint_id.clone();
                    move |sample| Input::Encoder {
                        joint_id: joint_id.clone(),
                        sample,
                    }
                },
            )
            .await?;

            publishers.insert(
                encoder.joint_id.clone(),
                io.publisher::<Stamped<JointState>>(&data::path(&encoder.joint_id))
                    .await?,
            );
        }

        let encoders = config
            .encoders
            .into_iter()
            .map(|encoder| (encoder.joint_id.clone(), encoder))
            .collect();

        Ok(Self {
            encoders,
            publishers,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let mut latest = BTreeMap::new();

        for input in inputs {
            match input {
                Input::Encoder { joint_id, sample } => {
                    latest.insert(joint_id, sample);
                }
            }
        }

        for (joint_id, sample) in latest {
            let Some(encoder) = self.encoders.get(&joint_id) else {
                warn!(joint_id = %joint_id, "joint runtime received input for unknown joint");
                continue;
            };
            let Some(publisher) = self.publishers.get(&joint_id) else {
                warn!(joint_id = %joint_id, "joint runtime has no publisher for joint");
                continue;
            };

            publisher
                .put(&Stamped::new(
                    step.tick.time_ns(),
                    JointState {
                        value: ticks_to_joint_rad(
                            sample.data.ticks(),
                            encoder.direction_sign,
                            encoder.gear_ratio,
                            encoder.counts_per_revolution,
                        ),
                        quantity: Quantity::AngleRad,
                    },
                ))
                .await?;
        }

        Ok(())
    }
}

fn ticks_to_joint_rad(
    ticks: i64,
    direction_sign: i8,
    gear_ratio: f64,
    counts_per_revolution: u32,
) -> f64 {
    f64::from(direction_sign) * TAU * ticks as f64 / f64::from(counts_per_revolution) / gear_ratio
}

#[cfg(test)]
mod tests {
    use super::ticks_to_joint_rad;

    const EPSILON: f64 = 1e-12;

    #[test]
    fn positive_ticks_produce_positive_joint_angle() {
        let ticks = 256;

        let joint_rad = ticks_to_joint_rad(ticks, 1, 1.0, 1024);

        assert_close(
            joint_rad,
            2.0 * std::f64::consts::PI * ticks as f64 / 1024.0,
        );
    }

    #[test]
    fn negative_direction_flips_joint_angle() {
        let ticks = 256;

        let joint_rad = ticks_to_joint_rad(ticks, -1, 1.0, 1024);

        assert_close(
            joint_rad,
            -(2.0 * std::f64::consts::PI * ticks as f64 / 1024.0),
        );
    }

    #[test]
    fn gear_ratio_divides_joint_angle() {
        let ticks = 256;

        let joint_rad = ticks_to_joint_rad(ticks, 1, 4.0, 1024);

        assert_close(
            joint_rad,
            2.0 * std::f64::consts::PI * ticks as f64 / 1024.0 / 4.0,
        );
    }

    #[test]
    fn zero_ticks_produces_zero_radians() {
        let joint_rad = ticks_to_joint_rad(0, 1, 4.0, 1024);

        assert_close(joint_rad, 0.0);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }
}
