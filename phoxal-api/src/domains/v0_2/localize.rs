//! v0.2 localization payloads.
#![allow(legacy_derive_helpers)]

use super::validation::{canonical_yaw, finite, finite_f32};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "LocalizationStateWire")]
pub struct LocalizationState {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
    pub confidence: f32,
}

impl LocalizationState {
    pub fn try_new(
        x_m: f64,
        y_m: f64,
        yaw_rad: f64,
        confidence: f32,
    ) -> Result<Self, &'static str> {
        if !finite(x_m)
            || !finite(y_m)
            || !canonical_yaw(yaw_rad)
            || !finite_f32(confidence)
            || !(0.0..=1.0).contains(&confidence)
        {
            return Err("localization state must contain finite bounded values and canonical yaw");
        }
        Ok(Self {
            x_m,
            y_m,
            yaw_rad,
            confidence,
        })
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalizationStateWire {
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    confidence: f32,
}

impl TryFrom<LocalizationStateWire> for LocalizationState {
    type Error = &'static str;
    fn try_from(value: LocalizationStateWire) -> Result<Self, Self::Error> {
        Self::try_new(value.x_m, value.y_m, value.yaw_rad, value.confidence)
    }
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
