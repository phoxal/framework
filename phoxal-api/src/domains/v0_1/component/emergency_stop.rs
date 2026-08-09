//! v0.1 component emergency_stop payloads.
#![allow(legacy_derive_helpers)]

/// Per-instance emergency-stop state.
#[derive(Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub engaged: bool,
}
