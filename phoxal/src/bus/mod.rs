//! The Phoxal bus ABI floor: the Zenoh-native wire boundary, plus the family,
//! payload, and endpoint-semantic primitives the bus client is generic over.
//!
//! Samples are Zenoh-native. One sample is:
//!
//! - a key, `phoxal/<execution-id>/<topic>`, where the topic is the
//!   family-rooted contract name;
//! - an encoding string naming the codec, plus a [`DeliveryMetadata`]
//!   attachment carrying codec, provenance, and optional Live attachment
//!   correlation - no schema, family, or api identity;
//! - a plain MessagePack body payload.
//!
//! There is no Phoxal frame independent of Zenoh and no version tag anywhere:
//! not in the key, not in the body. Identity lives entirely in the key:
//! different family-rooted contract names are different keys and physically
//! cannot collide, so a receiver's per-key subscription is the whole
//! fast-reject.
//!
//! # The frozen bootstrap-reachable subset
//!
//! Four of those facts are what an attaching client has to traverse *before* it
//! can decode the attachment bootstrap's reply and learn whether the two peers
//! agree at all: router discovery, the key grammar above, the query envelopes
//! ([`BusMetadata`] and [`QueryFailure`]), and the encoding string. Beneath them
//! sits the Zenoh wire protocol version, without which no session forms in the
//! first place. All five are preserved across framework majors, are emitted in
//! this module's contract surface, and are pinned by their own tests in the
//! modules that own them. A change to any of them is a bootstrap-breaking
//! event; see `xtask/README.md` "When a gate fails", rule 3 "A frozen bootstrap
//! fact drifted".
//!
//! # The module-root facade
//!
//! Every author-facing item below is re-exported from the module that owns it,
//! as one deliberate flat surface for consumers. The receive surface is
//! delivery-specific (`StateView`, `SetpointReceiver`, `SampleReceiver`, and
//! `StreamReceiver`); the generic ring implementations remain internal.
//! Code *inside* the framework always imports from the owning module, never
//! through this facade.
//!
//! Two things this module owns are absent from that surface on purpose, and are
//! named only by the modules that own them: `BusOwner` and `BusConfig` (opening
//! a session), and the embedded `Router`. No consumer profile receives raw
//! transport or fabric ownership - `phoxal::session`,
//! `phoxal::simulator` and `phoxal::supervisor::host` each own one on their
//! consumer's behalf.

pub mod abi;
pub mod contract;
pub mod error;
pub mod handle;
pub mod lease;
pub mod liveliness;
mod lock;
pub mod metadata;
pub mod query;
pub mod runtime_metrics;
pub mod server;
pub mod session;
pub mod time;
pub mod topic;
pub mod tree;

mod outbound;

#[cfg(test)]
mod test_support;

/// The runtime identities the bus carries.
///
/// They are owned by [`crate::identity`], which is where they are documented
/// and which is the path to name them by outside the bus. They are
/// re-exported here because they appear in this crate's own signatures
/// ([`SourceAttribution::producer`](metadata::SourceAttribution::producer),
/// [`RobotInstant::timeline`](time::RobotInstant::timeline)), so a caller
/// working against the bus should not have to reach for a second crate to name
/// what the bus hands it.
pub use crate::identity::{ExecutionId, ParticipantId, ProducerId, TimelineId};

pub use abi::{Codec, CodecError, CodecId, EncodingError, EncodingMetadata, MessagePack};
pub use contract::{
    DeliveryFamily, Direction, Endpoint, EndpointKind, EndpointSemantics, Event, Family, In, Out,
    Payload, Query, QueryEndpoint, Robot, RobotEndpoint, Runtime, Sample, Setpoint, Simulation,
    State, Stream, StreamDelivered, Supervisor, World,
};
pub use error::{BusError, KeyProblem, MetadataProblem, OutboundBound, Result, SessionIdRole};
pub use handle::publisher::{
    EventPublisher, SamplePublisher, SetpointPublisher, StatePublisher, StreamPublisher,
};
pub use handle::querier::{DEFAULT_QUERY_TIMEOUT, Querier};
pub use handle::stamp::{StepStamp, StepToken};
pub use handle::subscriber::{
    EventReceiver, MAX_SETPOINT_SOURCES, MAX_STREAM_SOURCES, Observed, ReceiveTerminal,
    SampleReceiver, SetpointReceiver, StateView, StreamEvent, StreamReceiver, TimelineRetention,
};
pub use lease::{
    ExclusiveProducerLease, FixedSourceAdmission, FixedSourceLease, LEASE_TRACE_TARGET,
    LeaseDecision, LeaseRejection, MAX_READY_PRODUCERS,
};
pub use liveliness::{
    KeyLivelinessObserver, KeyLivelinessToken, LivelinessStatus, ParticipantReadyEvent,
    ParticipantReadyEvents, ParticipantReadyObserver, ParticipantReadyStatus,
    ParticipantReadyToken,
};
pub use metadata::{
    BusMetadata, DeliveryMetadata, ParticipantSourceIdentity, SourceAttribution, SourceLabel,
    SourceLabelError, StreamPosition,
};
pub use query::{QueryCode, QueryError, QueryFailure, QueryResult};
pub use runtime_metrics::{
    RuntimeBufferKind, RuntimeDirection, RuntimeMetricKey, RuntimeMetricSnapshot,
};
pub use server::{IncomingQuery, ServerQueryable};
pub use session::{BusCloseReport, BusCloseTimeout, BusFault, BusHandle, BusHealth, BusTerminal};
/// Opening a session, and the inputs that open one.
///
/// Crate-private: owning the transport is what `phoxal::session`,
/// `phoxal::simulator` and the participant runner each do on their consumer's
/// behalf, and a second, unbranded copy of the transport beside them is exactly
/// what the typed handle vocabulary could then promise nothing about.
pub(crate) use session::{BusConfig, BusOwner};
pub use time::{
    CaptureStamp, LocalInstant, RetiredTimelines, RobotInstant, RobotTimeError, TimeWindow, Timed,
    TimelineMismatch, WallTimestamp,
};
pub use topic::{
    AskQuery, KeySegment, KeySegmentError, Publish, ServeQuery, Subscribe, Topic, TopicKind,
    WildcardPublish,
};
pub use tree::{BoundEndpoint, TopicSegment};

/// The contract surface this crate owns: the envelopes that ride beside a body
/// and the exact wire text a peer has to spell.
///
/// Not public API. It exists so compatibility CI can read this declared
/// process boundary out of the code that declares it.
///
/// Endpoints are deliberately absent: this module owns the transport floor, and
/// every concrete endpoint is declared beside its own payload by the api tree.
/// So are [`DeliveryFamily`] and [`EndpointKind`], which are compile-time
/// typing with no bytes of their own - they reach a surface only as the
/// spelling on an endpoint record, which is where they actually decide
/// something.
///
/// Most of what this module declares is bootstrap-reachable, so the
/// compatibility checker classifies it as the frozen bootstrap and routes any
/// drift in it to the stop rule. `participant-ready-key` is the exception: an
/// attaching client composes it only after the bootstrap has already told it
/// the two peers agree.
#[doc(hidden)]
pub mod __compat {
    use crate::__compat::surface::{ContractRecord, ContractSurface};
    use crate::__compat::wire::DescribeWire;

    use crate::bus::abi::CodecId;
    use crate::bus::liveliness::PARTICIPANT_LIVELINESS_PREFIX;
    use crate::bus::metadata::{BusMetadata, DeliveryMetadata};
    use crate::bus::query::QueryFailure;
    use crate::bus::session::{BUS_KEY_PREFIX, ZENOH_WIRE_PROTOCOL_VERSION};

    /// The canonical rendering of this module's own contract surface.
    #[must_use]
    pub fn contract_surface() -> String {
        let mut records = Vec::new();
        contract_records(&mut records);
        ContractSurface::new(records).canonical_json()
    }

    /// This module's records, for the crate aggregate.
    pub(crate) fn contract_records(out: &mut Vec<ContractRecord>) {
        out.extend([
            // The query attachment and common delivery base. This exact record
            // is frozen because attachment-bootstrap requests and replies use
            // it before compatibility is known.
            ContractRecord::envelope("BusMetadata", BusMetadata::wire_schema()),
            // Ordinary pub/sub samples add delivery-only attachment state in a
            // separate envelope, leaving the frozen query envelope unchanged.
            ContractRecord::envelope("DeliveryMetadata", DeliveryMetadata::wire_schema()),
            // A handler error rides Zenoh's native error reply leg with this
            // body, which no endpoint declaration mentions.
            ContractRecord::envelope("QueryFailure", QueryFailure::wire_schema()),
            // Every key this bus speaks is rooted at the execution, so a
            // previous run's traffic lands elsewhere and cannot be observed as
            // current.
            ContractRecord::identifier(
                "bus-key-root",
                format!("{BUS_KEY_PREFIX}/{{execution}}"),
            ),
            // The whole grammar, so a peer knows what goes below the root: one
            // family-rooted topic key and nothing between them.
            ContractRecord::identifier(
                "bus-key-composition",
                format!("{BUS_KEY_PREFIX}/{{execution}}/{{topic}}"),
            ),
            ContractRecord::identifier(
                "participant-ready-key",
                format!(
                    "{BUS_KEY_PREFIX}/{{execution}}/{PARTICIPANT_LIVELINESS_PREFIX}/{{participant}}/{{producer}}"
                ),
            ),
            // The encoding string a receiver validates before it decodes.
            ContractRecord::identifier("encoding", CodecId::MessagePack.encoding_string()),
            // The transport's own wire version. It is not a Phoxal spelling,
            // and it is recorded here because it is the floor everything above
            // stands on: peers that disagree on it never form a session, so
            // nothing above ever gets the chance to report the disagreement.
            ContractRecord::identifier(
                "zenoh-wire-protocol",
                ZENOH_WIRE_PROTOCOL_VERSION.to_string(),
            ),
        ]);
    }

    #[cfg(test)]
    mod tests {
        use super::contract_surface;

        /// The surface is one deterministic JSON document naming every wire
        /// fact this crate owns, so an accidentally empty or reordered surface
        /// cannot pass.
        #[test]
        fn the_surface_names_every_bus_owned_wire_fact_deterministically() {
            let rendered = contract_surface();
            serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
            assert_eq!(contract_surface(), rendered);
            for expected in [
                r#""name":"BusMetadata""#,
                r#""name":"DeliveryMetadata""#,
                r#""name":"QueryFailure""#,
                r#""value":"phoxal/{execution}""#,
                r#""value":"phoxal/{execution}/{topic}""#,
                r#""value":"phoxal/{execution}/liveliness/participants/{participant}/{producer}""#,
                r#""value":"phoxal/v0;codec=1""#,
                r#""name":"zenoh-wire-protocol","record":"identifier","value":"9""#,
                // The attachment's own fields, so an empty envelope body fails.
                r#""name":"produced_at""#,
            ] {
                assert!(
                    rendered.contains(expected),
                    "{expected} missing: {rendered}"
                );
            }
        }

        /// The declared envelope shapes are checked against real serialized
        /// values, which is what keeps the surface honest about the bytes the
        /// bus actually writes.
        #[test]
        fn the_declared_envelope_shapes_are_the_shapes_the_bus_writes() {
            use crate::__compat::wire::DescribeWire;
            use crate::identity::{ParticipantId, TimelineId};

            use crate::bus::abi::CodecId;
            use crate::bus::metadata::{
                BusMetadata, DeliveryMetadata, ParticipantSourceIdentity, SourceAttribution,
                StreamPosition,
            };
            use crate::bus::query::{QueryCode, QueryFailure};
            use crate::bus::test_support::producer;
            use crate::bus::time::{RobotInstant, TimeWindow};

            let metadata = BusMetadata {
                codec: CodecId::MessagePack.as_u8(),
                sequence: 3,
                stream_position: Some(StreamPosition { sequence: 1 }),
                produced_at: Some(TimeWindow::exact(RobotInstant::new(TimelineId::mint(), 8))),
                source: SourceAttribution::Participant(ParticipantSourceIdentity::new(
                    ParticipantId::new("drive").expect("a participant id"),
                    producer(1),
                )),
            };
            let json = serde_json::to_value(&metadata).expect("the attachment serializes");
            assert_eq!(BusMetadata::wire_schema().conforms(&json), Ok(()));

            let delivery = DeliveryMetadata::new(metadata, Some(2));
            let json = serde_json::to_value(&delivery).expect("delivery metadata serializes");
            assert_eq!(DeliveryMetadata::wire_schema().conforms(&json), Ok(()));

            let failure = QueryFailure::new(QueryCode::NotFound, "no such entity");
            let json = serde_json::to_value(&failure).expect("a query failure serializes");
            assert_eq!(QueryFailure::wire_schema().conforms(&json), Ok(()));
        }
    }
}
