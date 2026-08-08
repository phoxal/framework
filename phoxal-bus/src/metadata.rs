//! `BusMetadata` - the per-sample attachment.
//!
//! The wire body is the plain MessagePack payload; provenance rides here, in
//! the Zenoh attachment. Identity (which contract, which version) is not
//! carried in the envelope at all - it lives in the Zenoh key itself, since the
//! version is folded into `<Body as ContractBody>::TOPIC`, so a receiver's
//! per-key subscription is the whole fast-reject.
//!
//! Provenance is a [`SourceAttribution`] plus a per-producer sequence, and the
//! production instant is an explicit `Option<`[`TimeWindow`]`>` - a sample that
//! expresses no robot time carries `None`, never a sentinel. Participant and
//! producer identity are one source pair; external sources carry the producer
//! with an optional bounded diagnostic label. No admissibility decision reads
//! the diagnostic label.
//!
//! Receiver-side observation time is deliberately absent: it is process-local
//! and receiver-specific, so it belongs on
//! [`Observed`](crate::handle::subscriber::Observed), never on the wire.

use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};
use serde::{Deserialize, Serialize};

use crate::abi::CodecId;
use crate::time::{RobotInstant, TimeWindow};

const MAX_METADATA_BYTES: usize = 4 * 1024;
pub(crate) const MAX_SOURCE_LABEL_BYTES: usize = 512;

/// Bounded, non-authoritative source text for an external producer.
///
/// A label is useful in diagnostics and traces, but it is never used for
/// routing, authority, or Ready admission. Participant topology identity is
/// carried by [`ParticipantId`] instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SourceLabel(String);

impl SourceLabel {
    /// Construct a bounded diagnostic label.
    pub fn new(value: impl Into<String>) -> Result<Self, SourceLabelError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SOURCE_LABEL_BYTES {
            return Err(SourceLabelError(value));
        }
        Ok(Self(value))
    }

    /// The diagnostic text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceLabel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SourceLabel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A label that is empty or exceeds the diagnostic wire budget.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("source label must be non-empty and at most {MAX_SOURCE_LABEL_BYTES} bytes, got {0:?}")]
pub struct SourceLabelError(String);

/// The stable participant/source pair carried by Ready and sample provenance.
///
/// A participant id names the topology role while the producer id names the
/// concrete transport session incarnation. They are one source identity for
/// authority and freshness decisions; keeping them together prevents callers
/// from accidentally comparing one without the other.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ParticipantSourceIdentity {
    /// The compiled topology participant.
    pub participant: ParticipantId,
    /// The publishing bus-session incarnation.
    pub producer: ProducerId,
}

impl ParticipantSourceIdentity {
    /// Construct one participant/source pair.
    #[must_use]
    pub fn new(participant: ParticipantId, producer: ProducerId) -> Self {
        Self {
            participant,
            producer,
        }
    }
}

/// Provenance attribution for a bus session.
///
/// Every attribution carries a producer at the envelope level. Only compiled
/// participants receive a topology [`ParticipantId`]; attached/operator
/// sessions remain external and may carry a bounded diagnostic label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceAttribution {
    /// A compiled participant process.
    Participant(ParticipantSourceIdentity),
    /// An off-graph producer whose label is diagnostic only.
    External {
        /// The publishing bus-session incarnation.
        producer: ProducerId,
        /// Optional bounded diagnostic label.
        label: Option<SourceLabel>,
    },
}

impl SourceAttribution {
    /// The participant/source pair, if this is a compiled participant source.
    pub fn participant_source(&self) -> Option<&ParticipantSourceIdentity> {
        match self {
            Self::Participant(source) => Some(source),
            Self::External { .. } => None,
        }
    }

    /// The topology participant, if this is a compiled participant source.
    pub fn participant(&self) -> Option<&ParticipantId> {
        self.participant_source().map(|source| &source.participant)
    }

    /// The transport producer for this source.
    pub fn producer(&self) -> ProducerId {
        match self {
            Self::Participant(source) => source.producer,
            Self::External { producer, .. } => *producer,
        }
    }

    /// The optional external diagnostic label.
    pub fn label(&self) -> Option<&SourceLabel> {
        match self {
            Self::Participant(_) => None,
            Self::External { label, .. } => label.as_ref(),
        }
    }
}

/// Per-sample metadata carried in the Zenoh attachment (MessagePack-encoded).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusMetadata {
    /// The codec used for the body payload.
    pub codec: u8,
    /// This producer's monotonically increasing sample sequence, starting at
    /// zero for every fresh process.
    pub sequence: u64,
    /// When this sample's content was produced in robot time, if it expresses
    /// robot time at all. Commands and diagnostics carry `None`.
    pub produced_at: Option<TimeWindow>,
    /// The producing source, including its transport incarnation and optional
    /// topology/diagnostic attribution.
    pub source: SourceAttribution,
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
        let encoded = rmp_serde::to_vec_named(self)?;
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
            sequence: 7,
            produced_at,
            source: SourceAttribution::Participant(ParticipantSourceIdentity::new(
                ParticipantId::new("unit").expect("test participant"),
                producer(1),
            )),
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
    fn an_over_long_external_label_is_rejected_at_construction() {
        let mut long = metadata(None);
        assert!(SourceLabel::new("\u{e9}".repeat(MAX_SOURCE_LABEL_BYTES)).is_err());
        long.source = SourceAttribution::External {
            producer: producer(1),
            label: Some(SourceLabel::new("diagnostic").expect("label")),
        };
        assert!(BusMetadata::decode(&encoded(&long)).is_ok());
    }

    /// The attachment is a bounded wire value at both ends: an encoder that
    /// could exceed the limit, or a decoder that would accept an unbounded one,
    /// makes the bound advisory rather than real.
    #[test]
    fn the_attachment_stays_inside_its_wire_limit_in_both_directions() {
        let mut oversized = metadata(None);
        oversized.source = SourceAttribution::External {
            producer: producer(1),
            label: Some(SourceLabel::new("diagnostic").expect("label")),
        };
        let bytes = encoded(&oversized);
        assert!(bytes.len() <= MAX_METADATA_BYTES);
        let decoded = BusMetadata::decode(&bytes).expect("bounded metadata decodes");
        assert!(decoded.source.label().is_some());

        let error = BusMetadata::decode(&vec![0_u8; MAX_METADATA_BYTES + 1]).unwrap_err();
        assert!(error.to_string().contains("4096-byte limit"));
    }
}
