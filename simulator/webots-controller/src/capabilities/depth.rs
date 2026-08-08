//! Depth capability: publishes `component::depth::Frame` from the Webots
//! `RangeFinder` device.
//!
//! Webots reports metres as floats; the contract carries unsigned millimetres
//! with zero reserved for "no return", so the conversion below is where a
//! non-finite or non-positive reading becomes that reserved value rather than
//! a plausible-looking distance.

use anyhow::Result;
use phoxal::api;

use super::{SampledSpec, SensorStep, SimulatedSensor};

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
        let sensor = webots.range_finder(spec.sampled.reference.to_string())?;
        sensor.enable(spec.sampled.sampling_period_ms)?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
        })
    }
}

impl SimulatedSensor for NativeDepth {
    type Sample = api::component::depth::Frame;

    fn schedule(&mut self) -> &mut phoxal::SampleSchedule {
        &mut self.spec.sampled.schedule
    }

    fn read(&mut self, _step: SensorStep) -> Result<Option<Self::Sample>> {
        let samples_mm = self
            .sensor
            .get_range_image()?
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
