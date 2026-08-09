//! v0.1 component gnss payloads.
#![allow(legacy_derive_helpers)]

/// A GNSS fix: geodetic position plus a 3x3 position covariance.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: f64,
    pub position_covariance: [f64; 9],
}
