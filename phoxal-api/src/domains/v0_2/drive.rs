//! v0.2 drive payloads.
#![allow(legacy_derive_helpers)]


            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum StopReason {
                TargetStale,
                TargetNotFinite,
                ActuatorCommandNotFinite,
                EmergencyStop,
                Fault,
            }

            /// A finite requested or limited planar velocity in the current
            /// control wire revision.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            pub struct Target {
                #[serde(deserialize_with = "crate::deserialize_finite_target_scalar")]
                pub(crate) linear_x_mps: f32,
                #[serde(deserialize_with = "crate::deserialize_finite_target_scalar")]
                pub(crate) angular_z_radps: f32,
            }

            /// The drive participant's exclusive control state.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum State {
                Active {
                    target: Target,
                    limited_target: Target,
                },
                Stopped {
                    target: Target,
                    reason: StopReason,
                },
            }

