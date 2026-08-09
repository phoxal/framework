//! v0.1 component led payloads.
#![allow(legacy_derive_helpers)]

/// A per-LED on/off command.
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    On,
    Off,
}
