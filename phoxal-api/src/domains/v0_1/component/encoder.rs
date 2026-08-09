//! v0.1 component encoder payloads.
#![allow(legacy_derive_helpers)]

/// Per-encoder sample on a dynamic per-instance key.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub position_rad: f64,
    pub velocity_radps: f32,
}

impl Sample {
    pub const STALE_AFTER: std::time::Duration = std::time::Duration::from_millis(200);
}
