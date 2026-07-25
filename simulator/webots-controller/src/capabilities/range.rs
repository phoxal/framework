//! Range capability: publishes `component::range::Sample` from the Webots
//! `DistanceSensor` device. Moved from the monolith's `RangeSpec`
//! (main.rs:578-583) and `NativeRange` (main.rs:1327-1368).

use anyhow::{Result, anyhow};
use phoxal::api;

use super::{SampledSpec, is_due};

#[derive(Clone, Debug)]
pub(crate) struct RangeSpec {
    pub(crate) sampled: SampledSpec,
    pub(crate) min_range_m: f32,
    pub(crate) max_range_m: f32,
}

pub(crate) struct NativeRange {
    sensor: webots_rs::device::distance_sensor::DistanceSensor,
    spec: RangeSpec,
}

impl NativeRange {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &RangeSpec) -> Result<Self> {
        let sensor = webots
            .distance_sensor(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::range::Sample>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        Ok(Some(api::component::range::Sample {
            distance_m: self.sensor.value().map_err(|error| anyhow!(error))? as f32,
            limits: Some(api::component::range::Limits {
                min_m: self.spec.min_range_m,
                max_m: self.spec.max_range_m,
            }),
            quality: Some(api::component::range::SampleQuality {
                valid: true,
                confidence: None,
            }),
            health: api::component::range::SensorHealth::Nominal,
        }))
    }
}
