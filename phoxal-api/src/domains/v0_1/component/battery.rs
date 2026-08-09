//! v0.1 component battery payloads.
#![allow(legacy_derive_helpers)]

/// Battery state reported by the pack's owner - the simulator
/// backing this capability, or the real driver.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub voltage_v: f32,
    pub current_a: f32,
    pub charge_ratio: f32,
}
