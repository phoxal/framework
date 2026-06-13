use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::v1::component::capability::battery::State;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub voltage_v: f64,
    pub capacity_ah: f64,
}

pub struct Battery {
    sensor: webots_rs::device::battery_sensor::BatterySensor,
    capacity_joules: f64,
    voltage_v: f64,
    publish_every_steps: u64,
}

impl Battery {
    pub fn new(
        sensor: webots_rs::device::battery_sensor::BatterySensor,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        sensor
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            sensor,
            capacity_joules: config.capacity_ah * 3600.0 * config.voltage_v,
            voltage_v: config.voltage_v,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<State>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let current_energy_joules = self.sensor.get_value().map_err(|error| anyhow!(error))?;
        let percentage = if self.capacity_joules > 0.0 {
            ((current_energy_joules / self.capacity_joules) * 100.0) as f32
        } else {
            0.0
        };

        Ok(Some(State::new(self.voltage_v, 0.0, percentage)))
    }
}
