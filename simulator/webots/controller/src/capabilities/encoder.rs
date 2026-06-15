use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::encoder::v1::Sample;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderType {
    Incremental,
    Absolute,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
    pub gear_ratio: f64,
    pub counts_per_revolution: u32,
    pub encoder_type: EncoderType,
}

pub struct Encoder {
    sensor: webots_rs::device::position_sensor::PositionSensor,
    ticks_per_radian: f64,
    publish_every_steps: u64,
}

fn radians_to_ticks(radians: f64, ticks_per_radian: f64) -> i64 {
    (radians * ticks_per_radian).round() as i64
}

impl Encoder {
    pub fn new(
        sensor: webots_rs::device::position_sensor::PositionSensor,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        let _encoder_type = config.encoder_type;

        if config.gear_ratio <= f64::EPSILON {
            anyhow::bail!("gear_ratio must be > 0");
        }

        sensor
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            sensor,
            // Publish raw actuator-space encoder ticks. Runtime consumers apply
            // model encoder.direction_sign exactly once when they
            // convert ticks into joint or odometry motion.
            ticks_per_radian: (config.counts_per_revolution as f64 * config.gear_ratio)
                / std::f64::consts::TAU,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<Sample>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let radians = self.sensor.value().map_err(|error| anyhow!(error))?;
        Ok(Some(Sample::new(radians_to_ticks(
            radians,
            self.ticks_per_radian,
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::radians_to_ticks;

    #[test]
    fn encoder_ticks_do_not_apply_direction_sign_in_bridge() {
        let quarter_turn = std::f64::consts::FRAC_PI_2;
        let ticks_per_radian = 1024.0 / std::f64::consts::TAU;
        assert_eq!(radians_to_ticks(quarter_turn, ticks_per_radian), 256);
        assert_eq!(radians_to_ticks(-quarter_turn, ticks_per_radian), -256);
    }
}
