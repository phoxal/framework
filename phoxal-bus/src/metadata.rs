//! `BusMetadata` - the per-sample attachment (D43c/D62/D1).
//!
//! The wire body is the plain MessagePack payload (D62); provenance +
//! logical time ride here, in the Zenoh attachment. Identity (which contract,
//! which version) is no longer carried in the envelope at all - it lives in
//! the Zenoh key itself (the version is folded into
//! `<Body as ContractBody>::TOPIC`, D1), so a receiver's per-key subscription is
//! the whole fast-reject. `produced_at_ns`/`epoch`/`source` give logical-time +
//! causality.

use serde::{Deserialize, Serialize};

use crate::abi::CodecId;

/// Per-sample metadata carried in the Zenoh attachment (MessagePack-encoded).
///
/// Forward-compatibility: new fields are added as `Option`/defaulted so older
/// decoders ignore them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusMetadata {
    /// The codec used for the body payload.
    pub codec: u8,
    /// Logical production time in nanoseconds (the clock's `time_ns`).
    pub produced_at_ns: u64,
    /// The clock epoch the timestamp belongs to (reset/restart counter).
    pub epoch: u64,
    /// The producing participant + sequence.
    pub source: Source,
}

/// Producer identity + monotonic sequence (D43c).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The participant id (`ParticipantLaunch.participant_id`, never the static
    /// participant/artifact id - repeated component drivers must not collide, D53).
    pub participant: String,
    /// Per-process incarnation (bumped on restart).
    pub incarnation: u64,
    /// Per-producer monotonically increasing sample sequence.
    pub sequence: u64,
}

impl BusMetadata {
    /// Encode to the MessagePack attachment bytes.
    pub fn encode(&self) -> Vec<u8> {
        rmp_serde::to_vec_named(self).expect("BusMetadata is always serializable")
    }

    /// Decode from the MessagePack attachment bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(bytes)
    }

    /// The codec id, if recognized by this wire ABI.
    pub fn codec_id(&self) -> Option<CodecId> {
        CodecId::from_u8(self.codec)
    }
}
