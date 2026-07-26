//! Magnetometer capability: publishes `component::magnetometer::Sample` from
//! the Webots `Compass` device, which reports the world's north vector in the
//! sensor's own frame.

use anyhow::{Result, anyhow};
use phoxal::api;

use super::{SampledSpec, is_due};

pub(crate) struct NativeMagnetometer {
    compass: webots_rs::device::compass::Compass,
    spec: SampledSpec,
}

impl NativeMagnetometer {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &SampledSpec) -> Result<Self> {
        let compass = webots
            .compass(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        compass
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            compass,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::magnetometer::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let values = self.compass.values().map_err(|error| anyhow!(error))?;
        Ok(Some(api::component::magnetometer::Sample {
            magnetic_field: [values[0] as f32, values[1] as f32, values[2] as f32],
        }))
    }
}
