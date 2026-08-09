//! v0.1 component accelerometer payloads.
#![allow(legacy_derive_helpers)]

/// Raw accelerometer sample in the sensor-local frame in m/s^2.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub linear_acceleration: [f32; 3],
}
