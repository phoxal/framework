use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::accelerometer::v1::Sample;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Accelerometer {
    sensor: webots_rs::device::accelerometer::Accelerometer,
    publish_every_steps: u64,
}

impl Accelerometer {
    pub fn new(
        sensor: webots_rs::device::accelerometer::Accelerometer,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        sensor
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            sensor,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<Sample>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let values = self.sensor.values().map_err(|error| anyhow!(error))?;
        Ok(Some(Sample::new(values.map(|value| value as f32))))
    }
}

#[cfg(test)]
mod tests {

    use phoxal::api::component::capability::accelerometer::v1::Sample;

    #[test]
    fn raw_values_are_forwarded_without_zeroing() {
        let payload = Sample::new([0.01, 0.01, 0.01].map(|value| value as f32));
        assert_eq!(payload.linear_acceleration(), &[0.01, 0.01, 0.01]);
    }
}
