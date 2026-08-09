//! v0.2 motion payloads.
#![allow(legacy_derive_helpers)]

            #[allow(unused_imports)]
            pub use crate::domains::v0_1::motion::{
                ManualCommand, SafetyRuntime, Source, ZeroReason,
            };

            /// The sole motion execution decision. A stopped decision carries
            /// no source or target, so consumers cannot observe an active
            /// source alongside a stop reason.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Decision {
                Active {
                    source: Source,
                    target: crate::v0_2::drive::Target,
                },
                Stopped {
                    reason: ZeroReason,
                },
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub decision: Decision,
                /// How long ago motion observed the live manual command, on
                /// its own host clock. `None` when no manual command is live.
                pub manual_observed_age_ns: Option<u64>,
                pub autonomous_candidate_age_ns: Option<u64>,
                pub safety_constraints_age_ns: Option<u64>,
                pub safety_runtime: SafetyRuntime,
                pub component_estop_blocked: bool,
                pub active_safety_constraints: Vec<super::safety::Constraint>,
                pub safety_permission: super::safety::MotionPermission,
            }
