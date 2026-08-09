//! v0.1 component camera payloads.
#![allow(legacy_derive_helpers)]

                #[derive(Copy, Eq)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                #[serde(rename_all = "snake_case")]
                pub enum Encoding {
                    Jpeg,
                    Png,
                    L8,
                    Rgb8,
                    Rgba8,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct ExposureTiming {
                    pub exposure_start_ns: Option<u64>,
                    pub exposure_duration_ns: Option<u64>,
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct CalibrationIdentity {
                    pub id: String,
                    pub version: String,
                }

                /// One camera frame: encoded pixel bytes plus optional calibration
                /// and timing metadata.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Frame {
                    pub width: u32,
                    pub height: u32,
                    pub encoding: Encoding,
                    pub intrinsics: Option<Intrinsics>,
                    pub distortion: Option<Distortion>,
                    pub exposure: Option<ExposureTiming>,
                    pub calibration: Option<CalibrationIdentity>,
                    #[serde(with = "serde_bytes")]
                    pub data: Vec<u8>,
                }


