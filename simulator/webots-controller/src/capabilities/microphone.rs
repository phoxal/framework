//! Microphone capability: publishes `component::microphone::Frame` from the
//! Webots `Microphone` device. Webots hands back the raw encoded sample block
//! for the elapsed window, so the frame carries it through untouched.

use anyhow::{Result, anyhow};
use phoxal::api;

use super::{SampledSpec, is_due};

pub(crate) struct NativeMicrophone {
    microphone: webots_rs::device::microphone::Microphone,
    spec: SampledSpec,
}

impl NativeMicrophone {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &SampledSpec) -> Result<Self> {
        let microphone = webots
            .microphone(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        microphone
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            microphone,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::microphone::Frame>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let data = self
            .microphone
            .get_sample_data()
            .map_err(|error| anyhow!(error))?;
        // A silent window yields no samples; publishing an empty frame would
        // claim an observation the sensor did not make.
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(api::component::microphone::Frame { data }))
    }
}
