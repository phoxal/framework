use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::imu::v1::Sample;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Imu {
    inertial_unit: webots_rs::device::inertial_unit::InertialUnit,
    accelerometer: webots_rs::device::accelerometer::Accelerometer,
    gyro: webots_rs::device::gyro::Gyro,
    publish_every_steps: u64,
}

impl Imu {
    pub fn new(
        inertial_unit: webots_rs::device::inertial_unit::InertialUnit,
        accelerometer: webots_rs::device::accelerometer::Accelerometer,
        gyro: webots_rs::device::gyro::Gyro,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        inertial_unit
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;
        accelerometer
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;
        gyro.enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            inertial_unit,
            accelerometer,
            gyro,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<Sample>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let [roll, pitch, yaw] = self
            .inertial_unit
            .get_roll_pitch_yaw()
            .map_err(|error| anyhow!(error))?;
        let quaternion = quaternion_wxyz_from_rpy(roll, pitch, yaw);
        let linear_acceleration_mps2 = self
            .accelerometer
            .values()
            .map_err(|error| anyhow!(error))?
            .map(|value| value as f32);
        let angular_velocity_radps = self
            .gyro
            .values()
            .map_err(|error| anyhow!(error))?
            .map(|value| value as f32);

        Ok(Some(Sample::from_motion_with_orientation(
            angular_velocity_radps,
            linear_acceleration_mps2,
            quaternion,
        )))
    }
}

fn quaternion_wxyz_from_rpy(roll: f64, pitch: f64, yaw: f64) -> [f32; 4] {
    let half_roll = roll * 0.5;
    let half_pitch = pitch * 0.5;
    let half_yaw = yaw * 0.5;

    let (sr, cr) = half_roll.sin_cos();
    let (sp, cp) = half_pitch.sin_cos();
    let (sy, cy) = half_yaw.sin_cos();

    [
        (cr * cp * cy + sr * sp * sy) as f32,
        (sr * cp * cy - cr * sp * sy) as f32,
        (cr * sp * cy + sr * cp * sy) as f32,
        (cr * cp * sy - sr * sp * cy) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::quaternion_wxyz_from_rpy;
    use std::f64::consts::FRAC_PI_2;

    #[test]
    fn quaternion_from_yaw_is_wxyz() {
        let quaternion = quaternion_wxyz_from_rpy(0.0, 0.0, FRAC_PI_2);
        let half = (FRAC_PI_2 * 0.5).sin_cos();
        assert!((f64::from(quaternion[0]) - half.1).abs() < 1e-6);
        assert!(f64::from(quaternion[1]).abs() < 1e-6);
        assert!(f64::from(quaternion[2]).abs() < 1e-6);
        assert!((f64::from(quaternion[3]) - half.0).abs() < 1e-6);
    }
}
