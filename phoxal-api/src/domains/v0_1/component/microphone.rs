//! v0.1 component microphone payloads.
#![allow(legacy_derive_helpers)]

/// One audio frame as raw encoded bytes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub data: Vec<u8>,
}
