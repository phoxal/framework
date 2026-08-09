//! v0.1 odometry payloads.
#![allow(legacy_derive_helpers)]

/// A planar pose + twist estimate in the odometry frame.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
}
