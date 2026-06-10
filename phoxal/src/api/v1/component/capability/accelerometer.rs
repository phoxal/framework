use derive_new::new;
use serde::{Deserialize, Serialize};

/// Raw accelerometer sample in the sensor-local frame in m/s^2.
///
/// This payload does not guarantee gravity compensation, zero-bias removal,
/// or rest-state filtering. Small non-zero readings while stationary are valid
/// unless a specific producer documents additional normalization.
#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    linear_acceleration: [f32; 3],
}

impl Sample {
    pub const fn linear_acceleration(&self) -> &[f32; 3] {
        &self.linear_acceleration
    }
}

pub const KIND: &str = "accelerometer";
