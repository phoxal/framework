//! IMU capability: publishes `component::imu::Sample` from the Webots
//! `InertialUnit` + `Accelerometer` + `Gyro` devices. Moved from the
//! monolith's `NativeImu` (main.rs:1189-1257) and the
//! `quaternion_wxyz_from_rpy` helper (main.rs:1520-1533).

use anyhow::{Result, anyhow};
use phoxal::api;

use super::{SampledSpec, is_due};

pub(crate) type ImuSpec = SampledSpec;

pub(crate) struct NativeImu {
    inertial_unit: webots_rs::device::inertial_unit::InertialUnit,
    accelerometer: webots_rs::device::accelerometer::Accelerometer,
    gyro: webots_rs::device::gyro::Gyro,
    spec: ImuSpec,
}

impl NativeImu {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &ImuSpec) -> Result<Self> {
        let inertial_unit = webots
            .inertial_unit(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        let accelerometer = webots
            .accelerometer(format!("{}__accel", spec.reference))
            .map_err(|error| anyhow!(error))?;
        let gyro = webots
            .gyro(format!("{}__gyro", spec.reference))
            .map_err(|error| anyhow!(error))?;
        inertial_unit
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        accelerometer
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        gyro.enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            inertial_unit,
            accelerometer,
            gyro,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::imu::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let [roll, pitch, yaw] = self
            .inertial_unit
            .get_roll_pitch_yaw()
            .map_err(|error| anyhow!(error))?;
        let acceleration = self
            .accelerometer
            .values()
            .map_err(|error| anyhow!(error))?
            .map(|value| value as f32);
        let angular_velocity = self
            .gyro
            .values()
            .map_err(|error| anyhow!(error))?
            .map(|value| value as f32);
        Ok(Some(api::component::imu::Sample {
            orientation: Some(quaternion_wxyz_from_rpy(roll, pitch, yaw)),
            angular_velocity_radps: angular_velocity,
            linear_acceleration_mps2: acceleration,
            covariance: None,
            noise_density: None,
            sensor_frame_id: None,
            health: api::component::imu::SensorHealth::Nominal,
            bias: None,
        }))
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
    use super::*;

    #[test]
    fn quaternion_from_yaw_is_wxyz() {
        let quaternion = quaternion_wxyz_from_rpy(0.0, 0.0, std::f64::consts::FRAC_PI_2);
        let half = (std::f64::consts::FRAC_PI_2 * 0.5).sin_cos();
        assert!((f64::from(quaternion[0]) - half.1).abs() < 1e-6);
        assert!(f64::from(quaternion[1]).abs() < 1e-6);
        assert!(f64::from(quaternion[2]).abs() < 1e-6);
        assert!((f64::from(quaternion[3]) - half.0).abs() < 1e-6);
    }
}
