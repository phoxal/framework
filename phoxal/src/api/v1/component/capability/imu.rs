use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    orientation: Option<[f32; 4]>,
    angular_velocity_radps: [f32; 3],
    linear_acceleration_mps2: [f32; 3],
    covariance: Option<[f32; 9]>,
    noise_density: Option<[f32; 3]>,
    sensor_frame_id: Option<String>,
    measured_at_ns: Option<u64>,
    health: SensorHealth,
    bias: Option<Bias>,
}

impl Sample {
    pub fn new(orientation: [f32; 4]) -> Self {
        Self {
            orientation: Some(orientation),
            angular_velocity_radps: [0.0; 3],
            linear_acceleration_mps2: [0.0; 3],
            covariance: None,
            noise_density: None,
            sensor_frame_id: None,
            measured_at_ns: None,
            health: SensorHealth::Nominal,
            bias: None,
        }
    }

    pub fn from_motion(
        angular_velocity_radps: [f32; 3],
        linear_acceleration_mps2: [f32; 3],
    ) -> Self {
        Self {
            orientation: None,
            angular_velocity_radps,
            linear_acceleration_mps2,
            covariance: None,
            noise_density: None,
            sensor_frame_id: None,
            measured_at_ns: None,
            health: SensorHealth::Nominal,
            bias: None,
        }
    }

    pub fn from_motion_with_orientation(
        angular_velocity_radps: [f32; 3],
        linear_acceleration_mps2: [f32; 3],
        orientation: [f32; 4],
    ) -> Self {
        Self {
            orientation: Some(orientation),
            angular_velocity_radps,
            linear_acceleration_mps2,
            covariance: None,
            noise_density: None,
            sensor_frame_id: None,
            measured_at_ns: None,
            health: SensorHealth::Nominal,
            bias: None,
        }
    }

    pub const fn orientation(&self) -> Option<&[f32; 4]> {
        self.orientation.as_ref()
    }

    pub const fn angular_velocity_radps(&self) -> &[f32; 3] {
        &self.angular_velocity_radps
    }

    pub const fn linear_acceleration_mps2(&self) -> &[f32; 3] {
        &self.linear_acceleration_mps2
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bias {
    pub angular_velocity_radps: [f32; 3],
    pub linear_acceleration_mps2: [f32; 3],
}

pub const KIND: &str = "imu";

#[cfg(test)]
mod tests {
    use super::Sample;

    #[test]
    fn full_motion_orientation_sample_round_trips_through_accessors() {
        let angular_velocity_radps = [0.1, 0.2, 0.3];
        let linear_acceleration_mps2 = [1.0, 2.0, 9.81];
        let orientation = [1.0, 0.0, 0.0, 0.0];

        let sample = Sample::from_motion_with_orientation(
            angular_velocity_radps,
            linear_acceleration_mps2,
            orientation,
        );

        assert_eq!(sample.orientation(), Some(&orientation));
        assert_eq!(sample.angular_velocity_radps(), &angular_velocity_radps);
        assert_eq!(sample.linear_acceleration_mps2(), &linear_acceleration_mps2);
    }
}
