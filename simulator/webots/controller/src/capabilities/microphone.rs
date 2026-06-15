use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::microphone::v1::Frame;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Microphone {
    microphone: webots_rs::device::microphone::Microphone,
    publish_every_steps: u64,
}

impl Microphone {
    pub fn new(
        microphone: webots_rs::device::microphone::Microphone,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        microphone
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            microphone,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<Frame>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let data = self
            .microphone
            .get_sample_data()
            .map_err(|error| anyhow!(error))?;
        Ok(Some(Frame::new(data)))
    }
}
