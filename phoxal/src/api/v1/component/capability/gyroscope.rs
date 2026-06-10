use derive_new::new;
use serde::{Deserialize, Serialize};

/// Raw angular velocity sample in the sensor-local frame in rad/s.
///
/// This payload does not guarantee zero-bias removal or rest-state filtering.
/// Small non-zero readings while stationary are valid unless a specific producer
/// documents additional normalization.
#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    angular_velocity: [f32; 3],
}

impl Sample {
    pub const fn angular_velocity(&self) -> &[f32; 3] {
        &self.angular_velocity
    }
}

pub const KIND: &str = "gyroscope";
