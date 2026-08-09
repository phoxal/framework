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

impl LocalizationState {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.x_m.is_finite()
            && self.y_m.is_finite()
            && self.yaw_rad.is_finite()
            && self.confidence.is_finite()
            && self.confidence > 0.0
    }
}
