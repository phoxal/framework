use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::range::v1::Sample as RangeData;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Range {
    sensor: webots_rs::device::distance_sensor::DistanceSensor,
    publish_every_steps: u64,
}

impl Range {
    pub fn new(
        sensor: webots_rs::device::distance_sensor::DistanceSensor,
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

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<RangeData>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        Ok(Some(Self::payload(
            self.sensor.value().map_err(|error| anyhow!(error))? as f32,
        )))
    }

    fn payload(distance_m: f32) -> RangeData {
        RangeData::new(distance_m)
    }
}

#[cfg(test)]
mod tests {
    use super::Range;

    #[test]
    fn builds_distance_sensor_payload() {
        let payload = Range::payload(1.25);
        assert_eq!(payload.distance_m(), 1.25);
    }
}
