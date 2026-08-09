//! v0.1 map payloads.
#![allow(legacy_derive_helpers)]

            /// A published map revision marker.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Revision {
                pub revision: u64,
                pub resolution_m: f32,
            }

            /// Request a rectangular submap window (map-frame metres).
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct SubmapRequest {
                pub min_x_m: f64,
                pub min_y_m: f64,
                pub max_x_m: f64,
                pub max_y_m: f64,
            }

            /// An occupancy-grid window: row-major cells, 0..=100 + 255 = unknown.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct SubmapResponse {
                pub width: u32,
                pub height: u32,
                pub resolution_m: f32,
                pub cells: Vec<u8>,
            }


