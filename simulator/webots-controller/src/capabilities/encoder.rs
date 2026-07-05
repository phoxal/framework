//! Encoder capability: publishes `component::encoder::Sample` from the
//! Webots `PositionSensor` device. Moved from the monolith's `EncoderSpec`
//! (main.rs:563-569), `NativeEncoder` (main.rs:1142-1187), and the
//! joint/actuator position + velocity helpers (main.rs:1502-1518).

use anyhow::{Result, anyhow};
use phoxal::model::component::v1::CapabilityRef;
use phoxal_api::y2026_1 as api;

use super::is_due;

#[derive(Clone, Debug)]
pub(crate) struct EncoderSpec {
    pub(crate) reference: CapabilityRef,
    pub(crate) publish_every_steps: u64,
    pub(crate) sampling_period_ms: i32,
    pub(crate) gear_ratio: f64,
}

pub(crate) struct NativeEncoder {
    sensor: webots_rs::device::position_sensor::PositionSensor,
    spec: EncoderSpec,
    last: Option<(f64, u64)>,
}

impl NativeEncoder {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &EncoderSpec) -> Result<Self> {
        let sensor = webots
            .position_sensor(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
            last: None,
        })
    }

    pub(crate) fn read_if_due(
        &mut self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::encoder::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let position_rad = joint_to_actuator_position(
            self.sensor.value().map_err(|error| anyhow!(error))?,
            self.spec.gear_ratio,
        );
        let velocity_radps = self
            .last
            .map(|(last_position, last_time)| {
                velocity_radps(position_rad, last_position, time_ns, last_time)
            })
            .unwrap_or(0.0);
        self.last = Some((position_rad, time_ns));
        Ok(Some(api::component::encoder::Sample {
            position_rad,
            velocity_radps: velocity_radps as f32,
        }))
    }
}

fn joint_to_actuator_position(joint_position_rad: f64, gear_ratio: f64) -> f64 {
    joint_position_rad * gear_ratio
}

fn velocity_radps(
    position: f64,
    previous_position: f64,
    time_ns: u64,
    previous_time_ns: u64,
) -> f64 {
    let dt_ns = time_ns.saturating_sub(previous_time_ns);
    if dt_ns == 0 {
        0.0
    } else {
        (position - previous_position) * 1_000_000_000.0 / dt_ns as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint_to_actuator_scales_by_gear_ratio() {
        assert_eq!(joint_to_actuator_position(2.0, 2.0), 4.0);
    }

    #[test]
    fn velocity_is_position_delta_over_time_delta() {
        assert_eq!(velocity_radps(2.0, 1.0, 2_000_000_000, 1_000_000_000), 1.0);
    }
}
