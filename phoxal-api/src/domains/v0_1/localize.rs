//! v0.1 localize payloads.
#![allow(legacy_derive_helpers)]

/// A planar localization estimate in the map frame.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalizationState {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
    pub confidence: f32,
}
