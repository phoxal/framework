//! v0.1 frame payloads.
#![allow(legacy_derive_helpers)]

            /// A parent → child rigid transform (translation + xyzw quaternion).
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct FrameTransform {
                pub parent_frame_id: String,
                pub child_frame_id: String,
                pub translation_m: [f64; 3],
                pub rotation_quat_xyzw: [f64; 4],
                /// When this transform was observed. Absent for a static
                /// transform, which is configuration rather than observation.
                pub stamp: Option<::phoxal_bus::RobotInstant>,
            }

            /// Transforms that do not change over time.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct StaticTransforms {
                pub transforms: Vec<FrameTransform>,
            }

            /// The current transform tree.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Tree {
                pub transforms: Vec<FrameTransform>,
            }

            /// Ask for the transform between two frames, optionally at a time.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct LookupRequest {
                pub target_frame_id: String,
                pub source_frame_id: String,
                /// The instant to resolve at. The frame service returns the
                /// latest dynamic sample at or before this instant and never a
                /// future sample. Absent asks for the greatest retained time.
                pub at: Option<::phoxal_bus::RobotInstant>,
            }

            /// The resolved transform, or `None` if it is not available.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct LookupResponse {
                pub transform: Option<FrameTransform>,
            }


