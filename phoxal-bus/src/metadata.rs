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

const MAX_METADATA_BYTES: usize = 4 * 1024;
pub(crate) const MAX_SOURCE_PARTICIPANT_BYTES: usize = 512;

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
        let mut bounded = self.clone();
        if bounded.source.participant.len() > MAX_SOURCE_PARTICIPANT_BYTES {
            bounded.source.participant =
                truncate_utf8(&bounded.source.participant, MAX_SOURCE_PARTICIPANT_BYTES);
        }
        let encoded =
            rmp_serde::to_vec_named(&bounded).expect("BusMetadata is always serializable");
        debug_assert!(encoded.len() <= MAX_METADATA_BYTES);
        encoded
    }

    /// Decode from the MessagePack attachment bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(rmp_serde::decode::Error::Syntax(format!(
                "BusMetadata exceeds the {MAX_METADATA_BYTES}-byte limit"
            )));
        }
        rmp_serde::from_slice(bytes)
    }

    /// The codec id, if recognized by this wire ABI.
    pub fn codec_id(&self) -> Option<CodecId> {
        CodecId::from_u8(self.codec)
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
