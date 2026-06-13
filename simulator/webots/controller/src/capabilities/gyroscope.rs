use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::v1::component::capability::gyroscope::Sample;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Gyroscope {
    gyro: webots_rs::device::gyro::Gyro,
    publish_every_steps: u64,
}

impl Gyroscope {
    pub fn new(
        gyro: webots_rs::device::gyro::Gyro,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        gyro.enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            gyro,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<Sample>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let values = self.gyro.values().map_err(|error| anyhow!(error))?;
        Ok(Some(payload_from_values(values)))
    }
}

fn payload_from_values(values: [f64; 3]) -> Sample {
    Sample::new(values.map(|value| value as f32))
}

#[cfg(test)]
mod tests {
    use super::payload_from_values;

    #[test]
    fn raw_values_are_forwarded_without_zeroing() {
        let payload = payload_from_values([0.01, 0.01, 0.01]);
        assert_eq!(payload.angular_velocity(), &[0.01, 0.01, 0.01]);
    }
}
