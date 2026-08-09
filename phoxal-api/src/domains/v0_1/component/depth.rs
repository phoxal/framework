//! v0.1 component depth payloads.
#![allow(legacy_derive_helpers)]

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    U16Millimeters,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidSamplePolicy {
    ZeroIsInvalid,
    NonFiniteIsInvalid,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Distortion {
    pub model: String,
    pub coefficients: Vec<f32>,
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExposureTiming {
    pub exposure_start_ns: Option<u64>,
    pub exposure_duration_ns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CalibrationIdentity {
    pub id: String,
    pub version: String,
}

/// One depth frame: per-pixel millimetre samples plus optional
/// calibration and timing metadata.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub samples_mm: Vec<u16>,
    pub encoding: Encoding,
    pub invalid_sample_policy: InvalidSamplePolicy,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub intrinsics: Option<Intrinsics>,
    pub distortion: Option<Distortion>,
    pub exposure: Option<ExposureTiming>,
    pub calibration: Option<CalibrationIdentity>,
}
