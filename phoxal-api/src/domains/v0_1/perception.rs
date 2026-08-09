//! v0.1 perception payloads.
#![allow(legacy_derive_helpers)]

/// A single detected object: class, confidence, and pose in a frame.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Detection {
    pub class_id: String,
    pub confidence: f32,
    pub position_m: [f64; 3],
    pub frame_id: String,
    pub track_id: Option<u64>,
}

/// A batch of detections from one perception cycle.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Detections {
    pub detections: Vec<Detection>,
    /// The frame instant these detections were derived from.
    pub stamp: Option<::phoxal_bus::RobotInstant>,
}

/// The perception participant's published health.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub healthy: bool,
    pub detector: String,
}
