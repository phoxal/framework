//! `zed_f9p` - u-blox ZED-F9P GNSS component driver stub.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::prelude::*;

const STEP_HZ: f64 = 10.0;

pub struct Api {
    gnss: Vec<MeasurementPublisher<api::component::gnss::Sample>>,
}

pub struct ZedF9pState {
    gnss_divisors: Vec<u64>,
}

#[derive(Debug, Clone)]
struct GnssSlot {
    capability_id: String,
    divisor: u64,
}

#[phoxal::driver(state = ZedF9pState, api = Api)]
pub struct ZedF9p;

impl Participant for ZedF9p {
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
                if let Capability::Gnss(config) = capability {
                    validate_publish_rate(capability_id, config.publish_rate_hz)?;
                    let _coordinate_system = config.coordinate_system;
                    slots.push(GnssSlot {
                        capability_id: capability_id.to_string(),
                        divisor: divisor_for_rate(STEP_HZ, config.publish_rate_hz),
                    });
                }
            }

            slots
        };

        if slots.is_empty() {
            bail!("zed_f9p requires at least one gnss capability");
        }

        let mut gnss = Vec::new();
        let mut gnss_divisors = Vec::new();
        for slot in slots {
            gnss.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&instance)
                        .gnss(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            gnss_divisors.push(slot.divisor);
        }

        Ok((ZedF9pState { gnss_divisors }, Api { gnss }))
    }

    #[phoxal::step(hz = 10)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        let at = step.now();
        let step_index = step.step_index();

        for (publisher, divisor) in api.gnss.iter().zip(&state.gnss_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish(CaptureStamp::exact(at), gnss_sample())?;
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

fn gnss_sample() -> api::component::gnss::Sample {
    api::component::gnss::Sample {
        latitude: 0.0,
        longitude: 0.0,
        altitude: 0.0,
        position_covariance: [0.0; 9],
    }
}

#[cfg(test)]
mod tests {
    use super::{divisor_for_rate, gnss_sample};

    #[test]
    fn divisor_and_stub_sample_are_stable() {
        assert_eq!(divisor_for_rate(10.0, 10.0), 1);
        assert_eq!(divisor_for_rate(10.0, 5.0), 2);

        let sample = gnss_sample();
        assert_eq!(sample.latitude, 0.0);
        assert_eq!(sample.position_covariance, [0.0; 9]);
    }
}
