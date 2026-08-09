//! v0.1 component motor payloads.
#![allow(legacy_derive_helpers)]

/// A per-actuator command.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    Velocity(f32),
    Torque(f32),
    Stop,
}
