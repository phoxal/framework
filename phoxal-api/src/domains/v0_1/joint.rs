//! v0.1 joint payloads.
#![allow(legacy_derive_helpers)]

            /// Per-joint position/velocity (and optional effort) on a dynamic
            /// per-joint key.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct JointState {
                pub position_rad: f64,
                pub velocity_radps: f64,
                pub effort_nm: Option<f64>,
            }


