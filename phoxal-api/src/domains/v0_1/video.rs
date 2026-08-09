//! v0.1 video payloads.
#![allow(legacy_derive_helpers)]

/// Ask to open a video stream for one exact camera capability at an
/// optional size. The pre-v1 backend currently has no encoded
/// transport, so the response reports that outcome instead of
/// fabricating a stream identity or lifecycle.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpenRequest {
    pub source: crate::VideoSourceRef,
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
}

/// Why a requested video stream could not be opened.
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenOutcome {
    /// The source exists, but no encoder/transport backend exists
    /// in this train.
    Unsupported,
    /// No camera source is currently available.
    Unavailable,
}
