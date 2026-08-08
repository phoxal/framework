//! # phoxal-bus
//!
//! The Phoxal bus ABI floor: the Zenoh-native wire boundary, plus the two
//! contract primitive traits ([`ApiVersion`] / [`ContractBody`]) the bus client
//! is generic over.
//!
//! Samples are Zenoh-native. One sample is:
//!
//! - a key, `phoxal/<execution-id>/<version>/<topic>`, with the API version
//!   folded in;
//! - an encoding string naming the codec, plus a [`BusMetadata`] attachment
//!   carrying codec and provenance only - no schema, family, or api identity;
//! - a plain MessagePack body payload.
//!
//! There is no Phoxal frame independent of Zenoh and no version tag in the
//! body. Identity lives entirely in the key: different version-qualified
//! contract names are different keys and physically cannot collide, so a
//! receiver's per-key subscription is the whole fast-reject.
//!
//! # The crate-root facade
//!
//! Every item below is re-exported from the module that owns it, as one
//! deliberate flat surface for downstream crates. The owning module is the
//! canonical path - `phoxal_bus::handle::subscriber::Latest` and
//! `phoxal_bus::Latest` are the same type, and rustdoc documents it once, in
//! its own module. Code *inside* this crate always imports from the owning
//! module, never through this facade.

pub mod abi;
pub mod contract;
pub mod error;
pub mod handle;
pub mod lease;
pub mod liveliness;
mod lock;
pub mod metadata;
pub mod query;
#[cfg(feature = "router")]
pub mod router;
pub mod runtime_metrics;
pub mod server;
pub mod session;
pub mod time;
pub mod topic;

#[cfg(test)]
mod test_support;

/// The runtime identities the bus carries.
///
/// They are owned by `phoxal-runtime-contract`, which is where they are
/// documented and which is the path to name them by outside the bus. They are
/// re-exported here because they appear in this crate's own signatures
/// ([`BusConfig::execution`](session::BusConfig::execution),
/// [`BusMetadata::producer`](metadata::BusMetadata::producer),
/// [`RobotInstant::timeline`](time::RobotInstant::timeline)), so a caller
/// working against the bus should not have to reach for a second crate to name
/// what the bus hands it.
pub use phoxal_runtime_contract::identity::{ExecutionId, ParticipantId, ProducerId, TimelineId};

pub use abi::{Codec, CodecError, CodecId, EncodingError, EncodingMetadata, MessagePack};
pub use contract::{
    ApiVersion, CommandContract, ContractBody, DeliveryFamily, DiagnosticContract,
    MeasurementContract, StateContract, StreamContract, TopicRole, WorldClockContract,
};
pub use error::{BusError, KeyProblem, MetadataProblem, OutboundBound, Result, SessionIdRole};
pub use handle::publisher::{
    CommandPublisher, DiagnosticPublisher, MeasurementPublisher, StatePublisher, StreamPublisher,
    WorldClockPublisher,
};
pub use handle::querier::{DEFAULT_QUERY_TIMEOUT, Querier};
pub use handle::stamp::{StepStamp, StepToken, TimelineAuthority, WorldStepToken};
pub use handle::subscriber::{Latest, Observed, Subscriber};
pub use lease::{
    ExclusiveProducerLease, FixedSourceLease, LEASE_TRACE_TARGET, LeaseDecision, LeaseRejection,
    MAX_READY_PRODUCERS,
};
pub use liveliness::{
    KeyLivelinessObserver, LivelinessStatus, ParticipantReadyEvent, ParticipantReadyEvents,
    ParticipantReadyObserver, ParticipantReadyStatus, ParticipantReadyToken,
};
pub use metadata::{BusMetadata, SourceAttribution, SourceLabel, SourceLabelError};
pub use query::{QueryCode, QueryError, QueryFailure, QueryResult};
#[cfg(feature = "router")]
pub use router::{Router, RouterWatch};
pub use runtime_metrics::{
    RuntimeBufferKind, RuntimeDirection, RuntimeMetricKey, RuntimeMetricSnapshot,
};
pub use server::{IncomingQuery, ServerQueryable};
pub use session::{BusCloseReport, BusCloseTimeout, BusConfig, BusHandle, BusHealth, BusOwner};
pub use time::{
    CaptureStamp, LocalInstant, RetiredTimelines, RobotInstant, RobotTimeError, TimeWindow, Timed,
    TimelineMismatch, WallTimestamp,
};
pub use topic::{AskQuery, Publish, ServeQuery, Subscribe, Topic, TopicKind, WildcardPublish};
