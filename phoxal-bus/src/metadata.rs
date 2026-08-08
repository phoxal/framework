//! `BusMetadata` - the per-sample attachment.
//!
//! The wire body is the plain MessagePack payload; provenance rides here, in
//! the Zenoh attachment. Identity (which contract, which version) is not
//! carried in the envelope at all - it lives in the Zenoh key itself, since the
//! version is folded into `<Body as ContractBody>::TOPIC`, so a receiver's
//! per-key subscription is the whole fast-reject.
//!
//! Provenance is [`ProducerId`] plus a per-producer sequence, and the
//! production instant is an explicit `Option<`[`TimeWindow`]`>` - a sample that
//! expresses no robot time carries `None`, never a sentinel. The participant id
//! rides alongside as a diagnostic label; it is never identity, and no
//! admissibility decision reads it.
//!
//! Receiver-side observation time is deliberately absent: it is process-local
//! and receiver-specific, so it belongs on
//! [`Observed`](crate::handle::subscriber::Observed), never on the wire.

use phoxal_runtime_contract::identity::ProducerId;
use serde::{Deserialize, Serialize};

use crate::abi::{CodecId, truncate_utf8};
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
    ///
    /// Fallible rather than infallible: the participant label is caller-supplied
    /// and the production instant is a nested type, so "this can never fail" is
    /// a claim about data this type does not own. A failure here happens while
    /// *reporting* a sample, and panicking there would turn a lost attachment
    /// into a lost process.
    pub fn encode(&self) -> std::result::Result<Vec<u8>, rmp_serde::encode::Error> {
        let mut bounded = self.clone();
        if bounded.participant.len() > MAX_SOURCE_PARTICIPANT_BYTES {
            bounded.participant = truncate_utf8(&bounded.participant, MAX_SOURCE_PARTICIPANT_BYTES);
        }
        let encoded = rmp_serde::to_vec_named(&bounded)?;
        debug_assert!(encoded.len() <= MAX_METADATA_BYTES);
        Ok(encoded)
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

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_runtime_contract::identity::TimelineId;

    use crate::test_support::producer;

    fn metadata(produced_at: Option<TimeWindow>) -> BusMetadata {
        BusMetadata {
            codec: CodecId::MessagePack.as_u8(),
            producer: producer(1),
            sequence: 7,
            produced_at,
            participant: "unit".to_string(),
        }
    }

    fn encoded(metadata: &BusMetadata) -> Vec<u8> {
        metadata.encode().expect("test metadata encodes")
    }

    #[test]
    fn provenance_round_trips_through_the_attachment() {
        let original = metadata(Some(TimeWindow::exact(RobotInstant::new(
            TimelineId::mint(),
            42,
        ))));
        assert_eq!(BusMetadata::decode(&encoded(&original)).unwrap(), original);
    }

    #[test]
    fn absence_of_a_production_instant_round_trips_as_absence() {
        let original = metadata(None);
        let decoded = BusMetadata::decode(&encoded(&original)).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.produced_at, None);
        assert_eq!(decoded.produced_exactly_at(), None);
    }

    #[test]
    fn an_exact_production_instant_round_trips_without_collapsing_a_window() {
        let timeline = TimelineId::mint();
        let exact = metadata(Some(TimeWindow::exact(RobotInstant::new(timeline, 42))));
        let decoded = BusMetadata::decode(&encoded(&exact)).unwrap();
        assert_eq!(
            decoded.produced_exactly_at(),
            Some(RobotInstant::new(timeline, 42))
        );

        let window = TimeWindow::bounded(
            RobotInstant::new(timeline, 40),
            RobotInstant::new(timeline, 44),
        )
        .unwrap();
        let bounded = BusMetadata::decode(&encoded(&metadata(Some(window)))).unwrap();
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
        long.participant = "\u{e9}".repeat(MAX_SOURCE_PARTICIPANT_BYTES);
        let decoded = BusMetadata::decode(&encoded(&long)).unwrap();
        assert!(decoded.participant.len() <= MAX_SOURCE_PARTICIPANT_BYTES);
        assert!(decoded.participant.chars().all(|c| c == '\u{e9}'));
    }

    /// The attachment is a bounded wire value at both ends: an encoder that
    /// could exceed the limit, or a decoder that would accept an unbounded one,
    /// makes the bound advisory rather than real.
    #[test]
    fn the_attachment_stays_inside_its_wire_limit_in_both_directions() {
        let mut oversized = metadata(None);
        oversized.participant = "\u{e9}".repeat(10_000);
        let bytes = encoded(&oversized);
        assert!(bytes.len() <= MAX_METADATA_BYTES);
        let decoded = BusMetadata::decode(&bytes).expect("bounded metadata decodes");
        assert!(decoded.participant.len() <= MAX_SOURCE_PARTICIPANT_BYTES);

        let error = BusMetadata::decode(&vec![0_u8; MAX_METADATA_BYTES + 1]).unwrap_err();
        assert!(error.to_string().contains("4096-byte limit"));
    }
}
