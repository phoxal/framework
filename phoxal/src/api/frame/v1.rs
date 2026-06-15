use std::fmt;

use crate::api::joint::v1::JointId;
use crate::bus::zenoh::BusyResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FrameId(pub String);

impl FrameId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub revision: u64,
    pub frames: Vec<FrameLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLink {
    pub frame_id: FrameId,
    pub parent_frame_id: Option<FrameId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Static {
    pub transforms: Vec<FrameTransform>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameTransform {
    pub parent_frame_id: Option<FrameId>,
    pub child_frame_id: FrameId,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    Static,
    Joint { joint_id: JointId },
    Lookup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLookupRequest {
    pub parent_frame_id: FrameId,
    pub child_frame_id: FrameId,
    pub timestamp_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FrameLookupResponse {
    Ok {
        parent_frame_id: FrameId,
        child_frame_id: FrameId,
        timestamp_ns: u64,
        transform: FrameTransform,
    },
    UnknownFrame {
        frame_id: FrameId,
    },
    DisconnectedTree {
        parent_frame_id: FrameId,
        child_frame_id: FrameId,
    },
    ExtrapolationTooOld {
        oldest_available_ns: u64,
    },
    ExtrapolationTooNew {
        newest_available_ns: u64,
    },
    Busy,
}

impl BusyResponse for FrameLookupResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::BusyResponse;

    use super::FrameLookupResponse;

    #[test]
    fn lookup_response_busy_uses_busy_variant() {
        assert_eq!(FrameLookupResponse::busy(), FrameLookupResponse::Busy);
    }
}
