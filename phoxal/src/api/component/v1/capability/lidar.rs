use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scan {
    Ranges(Ranges),
    Points(Points),
}

impl TypedSchema for Scan {
    const SCHEMA_NAME: &'static str = "component/capability/lidar";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "lidar";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: Scan
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ranges {
    pub ranges: Vec<f32>,
    pub geometry: Option<ScanGeometry>,
    pub limits: Option<RangeLimits>,
    pub measured_at_ns: Option<u64>,
    pub quality: Option<ScanQuality>,
    pub health: SensorHealth,
}

impl Ranges {
    pub fn new(ranges: impl Into<Vec<f32>>) -> Self {
        Self {
            ranges: ranges.into(),
            geometry: None,
            limits: None,
            measured_at_ns: None,
            quality: None,
            health: SensorHealth::Nominal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Points {
    pub points: Vec<[f32; 3]>,
    pub limits: Option<RangeLimits>,
    pub measured_at_ns: Option<u64>,
    pub quality: Option<ScanQuality>,
    pub health: SensorHealth,
}

impl Points {
    pub fn new(points: impl Into<Vec<[f32; 3]>>) -> Self {
        Self {
            points: points.into(),
            limits: None,
            measured_at_ns: None,
            quality: None,
            health: SensorHealth::Nominal,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScanGeometry {
    pub angle_min_rad: f32,
    pub angle_increment_rad: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RangeLimits {
    pub min_m: f32,
    pub max_m: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScanQuality {
    pub valid_points: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Scan;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Scan::SCHEMA_NAME, "component/capability/lidar");
        assert_eq!(Scan::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("front_lidar", "scan"),
            "component/front_lidar/scan/profile/default"
        );
    }
}
