use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSystem {
    #[default]
    Local,
    Wgs84,
}

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    latitude: f64,
    longitude: f64,
    altitude: f64,
    position_covariance: [f64; 9],
}

impl Sample {
    pub const fn latitude(&self) -> f64 {
        self.latitude
    }

    pub const fn longitude(&self) -> f64 {
        self.longitude
    }

    pub const fn altitude(&self) -> f64 {
        self.altitude
    }

    pub const fn position_covariance(&self) -> &[f64; 9] {
        &self.position_covariance
    }
}

pub const KIND: &str = "gnss";

#[cfg(test)]
mod tests {
    use super::CoordinateSystem;

    #[test]
    fn coordinate_system_defaults_to_local() {
        assert_eq!(CoordinateSystem::default(), CoordinateSystem::Local);
    }
}
