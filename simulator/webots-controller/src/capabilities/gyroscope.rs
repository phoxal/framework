//! Gyroscope capability: publishes `component::gyroscope::Sample` from the
//! Webots `Gyro` device. Moved from the monolith's `NativeGyroscope`
//! (main.rs:1295-1325).

use anyhow::{Result, anyhow};
use phoxal::api;

use super::{SampledSpec, is_due};

pub(crate) type GyroscopeSpec = SampledSpec;

pub(crate) struct NativeGyroscope {
    gyro: webots_rs::device::gyro::Gyro,
    spec: GyroscopeSpec,
}

impl NativeGyroscope {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &GyroscopeSpec) -> Result<Self> {
        let gyro = webots
            .gyro(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        gyro.enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            gyro,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::gyroscope::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        Ok(Some(api::component::gyroscope::Sample {
            angular_velocity: self
                .gyro
                .values()
                .map_err(|error| anyhow!(error))?
                .map(|value| value as f32),
        }))
    }
}
