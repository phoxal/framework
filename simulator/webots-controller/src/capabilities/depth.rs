//! Depth capability: publishes `component::depth::Frame` from the Webots
//! `RangeFinder` device. Moved from the monolith's `DepthSpec`
//! (main.rs:593-598), `NativeDepth` (main.rs:1416-1463), and the
//! `meters_to_u16_mm` helper (main.rs:1553-1559).

use anyhow::{Result, anyhow};
use phoxal::api;

use super::{SampledSpec, is_due};

#[derive(Clone, Debug)]
pub(crate) struct DepthSpec {
    pub(crate) sampled: SampledSpec,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) struct NativeDepth {
    sensor: webots_rs::device::range_finder::RangeFinder,
    spec: DepthSpec,
}

impl NativeDepth {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &DepthSpec) -> Result<Self> {
        let sensor = webots
            .range_finder(spec.sampled.reference.to_string())
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
    ) -> Result<Option<api::component::depth::Frame>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let samples_mm = self
            .sensor
            .get_range_image()
            .map_err(|error| anyhow!(error))?
            .into_iter()
            .map(meters_to_u16_mm)
            .collect();
        Ok(Some(api::component::depth::Frame {
            samples_mm,
            encoding: api::component::depth::Encoding::U16Millimeters,
            invalid_sample_policy: api::component::depth::InvalidSamplePolicy::ZeroIsInvalid,
            width: Some(self.spec.width),
            height: Some(self.spec.height),
            intrinsics: None,
            distortion: None,
            exposure: None,
            calibration: None,
        }))
    }
}

fn meters_to_u16_mm(meters: f32) -> u16 {
    if !meters.is_finite() || meters <= 0.0 {
        0
    } else {
        (meters * 1000.0).round().clamp(1.0, f32::from(u16::MAX)) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_to_u16_mm_rounds_and_clamps() {
        assert_eq!(meters_to_u16_mm(1.25), 1250);
        assert_eq!(meters_to_u16_mm(f32::NAN), 0);
    }
}
