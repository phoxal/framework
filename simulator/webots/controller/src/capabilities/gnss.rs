use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::v1::component::capability::gnss::Sample;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Gnss {
    gps: webots_rs::device::gps::Gps,
    publish_every_steps: u64,
}

impl Gnss {
    pub fn new(
        gps: webots_rs::device::gps::Gps,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        gps.enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            gps,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<Sample>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let reading = self.gps.reading().map_err(|error| anyhow!(error))?;
        Ok(Some(Sample::new(
            reading.position[0],
            reading.position[1],
            reading.position[2],
            [0.0; 9],
        )))
    }
}
