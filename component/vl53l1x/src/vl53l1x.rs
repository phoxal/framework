//! `vl53l1x` - VL53L1X range component driver stub.

use anyhow::{Result, anyhow, bail};
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityId;
use phoxal::prelude::*;

/// The fixed step cadence every capability schedule below divides.
const STEP_HZ: f64 = 20.0;

pub(crate) struct Api {
    range: Vec<MeasurementPublisher<api::component::range::Sample>>,
}

pub(crate) struct Vl53l1xState {
    range_specs: Vec<RangeSpec>,
}

/// One declared range capability and the spec its publisher is driven by.
///
/// The publishers are acquired in the order these are collected, so the spec at
/// index `i` belongs to the publisher at index `i`.
#[derive(Debug, Clone)]
struct RangeSlot {
    capability_id: CapabilityId,
    spec: RangeSpec,
}

#[derive(Debug, Clone)]
struct RangeSpec {
    schedule: SampleSchedule,
    min_range_m: f32,
    max_range_m: f32,
}

impl RangeSpec {
    /// The sample this driver reports for the capability, reading its declared
    /// limits rather than a device.
    fn sample(&self) -> api::component::range::Sample {
        api::component::range::Sample {
            distance_m: self.max_range_m,
            limits: Some(api::component::range::Limits {
                min_m: self.min_range_m,
                max_m: self.max_range_m,
            }),
            quality: None,
            health: api::component::range::SensorHealth::Nominal,
        }
    }
}

#[phoxal::driver(state = Vl53l1xState, api = Api)]
pub(crate) struct Vl53l1x;

impl Participant for Vl53l1x {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let instance = ctx.component()?.id().clone();
        let slots = {
            let robot = ctx.robot()?;
            let spec = robot
                .component_for_instance(instance.as_str())
                .ok_or_else(|| anyhow!("no component type is loaded for instance '{instance}'"))?;
            let mut slots = Vec::new();

            for (capability_id, capability) in spec.capabilities() {
                if let Capability::Range(config) = capability {
                    slots.push(RangeSlot {
                        capability_id: capability_id.clone(),
                        spec: RangeSpec {
                            schedule: SampleSchedule::new(
                                capability_id.as_str(),
                                STEP_HZ,
                                config.publish_rate_hz,
                            )?,
                            min_range_m: config.min_range_m as f32,
                            max_range_m: config.max_range_m as f32,
                        },
                    });
                }
            }

            slots
        };

        if slots.is_empty() {
            bail!("vl53l1x requires at least one range capability");
        }

        let mut range = Vec::new();
        let mut range_specs = Vec::new();
        for slot in slots {
            range.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .range(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            range_specs.push(slot.spec);
        }

        Ok((Vl53l1xState { range_specs }, Api { range }))
    }

    #[phoxal::step(hz = 20)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        let at = step.now();
        let step_index = step.step_index;

        for (publisher, spec) in api.range.iter().zip(&state.range_specs) {
            if spec.schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), spec.sample())?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::RangeSpec;
    use phoxal::SampleSchedule;

    #[test]
    fn the_reported_distance_follows_the_declared_range_limits() {
        let spec = RangeSpec {
            schedule: SampleSchedule::new("range", super::STEP_HZ, super::STEP_HZ)
                .expect("the step cadence is a valid publish rate"),
            min_range_m: 0.1,
            max_range_m: 4.0,
        };

        let sample = spec.sample();
        assert_eq!(sample.distance_m, 4.0);
        let limits = sample
            .limits
            .expect("the sample carries the declared limits");
        assert_eq!(limits.min_m, 0.1);
        assert_eq!(limits.max_m, 4.0);
    }
}
