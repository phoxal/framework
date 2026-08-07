//! `zed_f9p` - u-blox ZED-F9P GNSS component driver stub.

use anyhow::{Result, anyhow, bail};
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityId;
use phoxal::prelude::*;

/// The fixed step cadence every capability schedule below divides.
const STEP_HZ: f64 = 10.0;

pub(crate) struct Api {
    gnss: Vec<MeasurementPublisher<api::component::gnss::Sample>>,
}

pub(crate) struct ZedF9pState {
    gnss_schedules: Vec<SampleSchedule>,
}

/// One declared gnss capability and the cadence it publishes at.
///
/// The publishers are acquired in the order these are collected, so the
/// schedule at index `i` belongs to the publisher at index `i`.
#[derive(Debug, Clone)]
struct GnssSlot {
    capability_id: CapabilityId,
    schedule: SampleSchedule,
}

#[phoxal::driver(state = ZedF9pState, api = Api)]
pub(crate) struct ZedF9p;

impl Participant for ZedF9p {
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
                if let Capability::Gnss(config) = capability {
                    slots.push(GnssSlot {
                        capability_id: capability_id.clone(),
                        schedule: SampleSchedule::new(
                            capability_id.as_str(),
                            STEP_HZ,
                            config.publish_rate_hz,
                        )?,
                    });
                }
            }

            slots
        };

        if slots.is_empty() {
            bail!("zed_f9p requires at least one gnss capability");
        }

        let mut gnss = Vec::new();
        let mut gnss_schedules = Vec::new();
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
            gnss_schedules.push(slot.schedule);
        }

        Ok((ZedF9pState { gnss_schedules }, Api { gnss }))
    }

    #[phoxal::step(hz = 10)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        let at = step.now();
        let step_index = step.step_index;

        for (publisher, schedule) in api.gnss.iter().zip(&state.gnss_schedules) {
            if schedule.is_due(step_index) {
                publisher.publish(CaptureStamp::exact(at), gnss_sample())?;
            }
        }

        Ok(())
    }
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
    use super::gnss_sample;

    #[test]
    fn the_reported_fix_is_the_null_island_origin() {
        let sample = gnss_sample();
        assert_eq!(sample.latitude, 0.0);
        assert_eq!(sample.longitude, 0.0);
        assert_eq!(sample.altitude, 0.0);
        assert_eq!(sample.position_covariance, [0.0; 9]);
    }
}
