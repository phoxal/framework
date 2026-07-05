//! Accelerometer capability: publishes `component::accelerometer::Sample`
//! from the Webots `Accelerometer` device. Moved from the monolith's
//! `NativeAccelerometer` (main.rs:1259-1293).

use anyhow::{Result, anyhow};
use phoxal_api::y2026_1 as api;

use super::{SampledSpec, is_due};

pub(crate) type AccelerometerSpec = SampledSpec;

pub(crate) struct NativeAccelerometer {
    sensor: webots_rs::device::accelerometer::Accelerometer,
    spec: AccelerometerSpec,
}

impl NativeAccelerometer {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &AccelerometerSpec) -> Result<Self> {
        let sensor = webots
            .accelerometer(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::accelerometer::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        Ok(Some(api::component::accelerometer::Sample {
            linear_acceleration: self
                .sensor
                .values()
                .map_err(|error| anyhow!(error))?
                .map(|value| value as f32),
        }))
    }
}
