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
