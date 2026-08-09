//! v0.1 component range payloads.
#![allow(legacy_derive_helpers)]

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Limits {
    pub min_m: f32,
    pub max_m: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SampleQuality {
    pub valid: bool,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub distance_m: f32,
    pub limits: Option<Limits>,
    pub quality: Option<SampleQuality>,
    pub health: SensorHealth,
}
