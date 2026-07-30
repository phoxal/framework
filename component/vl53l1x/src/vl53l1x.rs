//! `vl53l1x` - VL53L1X range component driver stub.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::prelude::*;

const STEP_HZ: f64 = 20.0;

pub struct Api {
    range: Vec<MeasurementPublisher<api::component::range::Sample>>,
}

pub struct Vl53l1xState {
    range_specs: Vec<RangeSpec>,
}

#[derive(Debug, Clone)]
struct RangeSlot {
    capability_id: String,
    spec: RangeSpec,
}

#[derive(Debug, Clone)]
struct RangeSpec {
    divisor: u64,
    min_range_m: f32,
    max_range_m: f32,
}

#[phoxal::driver(state = Vl53l1xState, api = Api)]
pub struct Vl53l1x;

impl Participant for Vl53l1x {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let instance = ctx.component()?.id().to_string();
        let slots = {
            let robot = ctx.robot()?;
            let spec = robot.component_for_instance(&instance)?;
            let mut slots = Vec::new();

            for (capability_id, capability) in spec.capabilities() {
                if let Capability::Range(config) = capability {
                    validate_publish_rate(capability_id, config.publish_rate_hz)?;
                    slots.push(RangeSlot {
                        capability_id: capability_id.to_string(),
                        spec: RangeSpec {
                            divisor: divisor_for_rate(STEP_HZ, config.publish_rate_hz),
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
        let step_index = step.step_index();

        for (publisher, spec) in api.range.iter().zip(&state.range_specs) {
            if is_due(step_index, spec.divisor) {
                publisher.publish(CaptureStamp::exact(at), range_sample(spec))?;
            }
        }

        Ok(())
    }
}

fn validate_publish_rate(capability_id: &str, publish_rate_hz: f64) -> Result<()> {
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        bail!("capability '{capability_id}' publish_rate_hz must be > 0");
    }
    Ok(())
}

fn divisor_for_rate(step_hz: f64, publish_rate_hz: f64) -> u64 {
    (step_hz / publish_rate_hz).round().max(1.0) as u64
}

fn is_due(step_index: u64, divisor: u64) -> bool {
    divisor <= 1 || step_index.is_multiple_of(divisor)
}

fn range_sample(spec: &RangeSpec) -> api::component::range::Sample {
    api::component::range::Sample {
        distance_m: spec.max_range_m,
        limits: Some(api::component::range::Limits {
            min_m: spec.min_range_m,
            max_m: spec.max_range_m,
        }),
        quality: None,
        health: api::component::range::SensorHealth::Nominal,
    }
}

#[cfg(test)]
mod tests {
    use super::{divisor_for_rate, range_sample};

    #[test]
    fn divisor_and_stub_distance_follow_range_config() {
        assert_eq!(divisor_for_rate(20.0, 20.0), 1);
        assert_eq!(divisor_for_rate(20.0, 10.0), 2);

        let spec = super::RangeSpec {
            divisor: 1,
            min_range_m: 0.1,
            max_range_m: 4.0,
        };
        let sample = range_sample(&spec);
        assert_eq!(sample.distance_m, 4.0);
        assert_eq!(sample.limits.unwrap().min_m, 0.1);
    }
}
