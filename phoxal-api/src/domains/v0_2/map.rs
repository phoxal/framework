//! v0.2 map payloads.
#![allow(legacy_derive_helpers)]

#[allow(unused_imports)]
pub use crate::domains::v0_1::map::{Revision, SubmapRequest};

            /// A finite world-space point used as the cell origin and pose
            /// translation in a self-describing grid response.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::GridPointWire")]
            pub struct Point {
                pub x_m: f64,
                pub y_m: f64,
            }

            /// The map-frame pose of the grid's reference origin.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::GridPoseWire")]
            pub struct Pose {
                pub x_m: f64,
                pub y_m: f64,
                pub yaw_rad: f64,
            }

            /// Requested and covered map-frame bounds.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::GridBoundsWire")]
            pub struct Bounds {
                pub min_x_m: f64,
                pub min_y_m: f64,
                pub max_x_m: f64,
                pub max_y_m: f64,
            }

            /// Occupancy has a closed wire domain. Unknown is not treated as
            /// free by safety or navigation.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum Occupancy {
                Free,
                Occupied,
                Unknown,
            }

            /// A revisioned map window whose origin, frame, extent and bounds
            /// travel with the cells themselves.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::GridWindowWire")]
            pub struct GridWindow {
                pub frame_id: String,
                pub origin_pose: Pose,
                pub cell_origin: Point,
                #[serde(deserialize_with = "crate::deserialize_finite_positive_resolution")]
                pub resolution_m: f32,
                #[serde(deserialize_with = "crate::deserialize_nonzero_map_dimension")]
                pub width: u32,
                #[serde(deserialize_with = "crate::deserialize_nonzero_map_dimension")]
                pub height: u32,
                pub cells: Vec<Occupancy>,
                pub revision: u64,
                pub requested: Bounds,
                pub covered: Bounds,
            }

            /// A query either returns a complete window, a clipped window
            /// with explicit requested/covered bounds, or an explicit
            /// out-of-bounds result. A responder may not silently substitute
            /// a different extent for what was requested.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum SubmapResponse {
                Window(GridWindow),
                Partial { window: GridWindow },
                OutOfBounds {
                    requested: Bounds,
                    #[serde(deserialize_with = "crate::deserialize_nonempty_frame_id")]
                    frame_id: String,
                    revision: u64,
                },
            }
