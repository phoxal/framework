//! `BusMetadata` - the per-sample attachment (D43c/D62/D1).
//!
//! The wire body is the plain MessagePack payload (D62); provenance rides here,
//! in the Zenoh attachment. Identity (which contract, which version) is not
//! carried in the envelope at all - it lives in the Zenoh key itself (the
//! version is folded into `<Body as ContractBody>::TOPIC`, D1), so a receiver's
//! per-key subscription is the whole fast-reject.
//!
//! Provenance is [`ProducerId`] plus a per-producer sequence, and the
//! production instant is an explicit `Option<`[`TimeWindow`]`>` - a sample that
//! expresses no robot time carries `None`, never a sentinel. The participant id
//! rides alongside as a diagnostic label; it is never identity, and no
//! admissibility decision reads it.
//!
//! Receiver-side observation time is deliberately absent: it is process-local
//! and receiver-specific, so it belongs on [`Observed`](crate::handle::Observed),
//! never on the wire.

use serde::{Deserialize, Serialize};

use crate::abi::CodecId;
use crate::identity::ProducerId;
use crate::time::{RobotInstant, TimeWindow};

const MAX_METADATA_BYTES: usize = 4 * 1024;
pub(crate) const MAX_SOURCE_PARTICIPANT_BYTES: usize = 512;

/// Per-sample metadata carried in the Zenoh attachment (MessagePack-encoded).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusMetadata {
    /// The codec used for the body payload.
    pub codec: u8,
    /// The producing process.
    pub producer: ProducerId,
    /// This producer's monotonically increasing sample sequence, starting at
    /// zero for every fresh process.
    pub sequence: u64,
    /// When this sample's content was produced in robot time, if it expresses
    /// robot time at all. Commands and diagnostics carry `None`.
    pub produced_at: Option<TimeWindow>,
    /// The producing participant id - a diagnostic label only (never identity,
    /// never an admissibility input).
    pub participant: String,
}

impl BusMetadata {
    /// Encode to the MessagePack attachment bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut bounded = self.clone();
        if bounded.participant.len() > MAX_SOURCE_PARTICIPANT_BYTES {
            bounded.participant = truncate_utf8(&bounded.participant, MAX_SOURCE_PARTICIPANT_BYTES);
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

    /// The production instant when it is exactly known.
    ///
    /// A state sample published at a logical step is exact; a measurement
    /// translated from a device clock generally is not, and a consumer that
    /// needs an exact instant from one has to say so.
    pub fn produced_exactly_at(&self) -> Option<RobotInstant> {
        self.produced_at.and_then(TimeWindow::as_exact)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::TimelineId;

    fn metadata(produced_at: Option<TimeWindow>) -> BusMetadata {
        BusMetadata {
            codec: CodecId::MessagePack.as_u8(),
            producer: ProducerId::mint(),
            sequence: 7,
            produced_at,
            participant: "unit".to_string(),
        }
    }

    #[test]
    fn absence_of_a_production_instant_round_trips_as_absence() {
        let original = metadata(None);
        let decoded = BusMetadata::decode(&original.encode()).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.produced_at, None);
        assert_eq!(decoded.produced_exactly_at(), None);
    }

    #[test]
    fn an_exact_production_instant_round_trips_without_collapsing_a_window() {
        let timeline = TimelineId::mint();
        let exact = metadata(Some(TimeWindow::exact(RobotInstant::new(timeline, 42))));
        let decoded = BusMetadata::decode(&exact.encode()).unwrap();
        assert_eq!(
            decoded.produced_exactly_at(),
            Some(RobotInstant::new(timeline, 42))
        );

        let window = TimeWindow::bounded(
            RobotInstant::new(timeline, 40),
            RobotInstant::new(timeline, 44),
        )
        .unwrap();
        let bounded = BusMetadata::decode(&metadata(Some(window)).encode()).unwrap();
        assert_eq!(bounded.produced_at, Some(window));
        assert_eq!(
            bounded.produced_exactly_at(),
            None,
            "a bounded estimate must not present itself as exact"
        );
    }

    #[test]
    fn an_over_long_participant_label_is_truncated_at_a_char_boundary() {
        let mut long = metadata(None);
        long.participant = "é".repeat(MAX_SOURCE_PARTICIPANT_BYTES);
        let decoded = BusMetadata::decode(&long.encode()).unwrap();
        assert!(decoded.participant.len() <= MAX_SOURCE_PARTICIPANT_BYTES);
        assert!(decoded.participant.chars().all(|c| c == 'é'));
    }
}
