//! v0.1 component mmwave payloads.
#![allow(legacy_derive_helpers)]

                /// One mmWave radar detection: position, velocity, and SNR.
                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Detection {
                    pub position: [f32; 3],
                    pub velocity: [f32; 3],
                    pub snr: f32,
                }

                /// One mmWave radar scan as a set of detections.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Scan {
                    pub detections: Vec<Detection>,
                }


