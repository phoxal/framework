//! v0.1 component lidar payloads.
#![allow(legacy_derive_helpers)]

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScanGeometry {
    pub angle_min_rad: f32,
    pub angle_increment_rad: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RangeLimits {
    pub min_m: f32,
    pub max_m: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScanQuality {
    pub valid_points: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ranges {
    pub ranges: Vec<f32>,
    pub geometry: Option<ScanGeometry>,
    pub limits: Option<RangeLimits>,
    pub quality: Option<ScanQuality>,
    pub health: SensorHealth,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Points {
    pub points: Vec<[f32; 3]>,
    pub limits: Option<RangeLimits>,
    pub quality: Option<ScanQuality>,
    pub health: SensorHealth,
}

/// One lidar scan, either as polar ranges or as cartesian points.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scan {
    Ranges(Ranges),
    Points(Points),
}
