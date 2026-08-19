//! The bus-layer error type.
//!
//! One crate-wide error is the right shape here because every fallible bus
//! operation - open, publish, subscribe, query, liveliness - fails through the
//! same small set of conditions, and a caller that handles one of them handles
//! it the same way whichever call produced it. The variants below carry typed
//! data wherever this crate owns the type; the two places that stay free text
//! ([`BusError::Transport`] and [`KeyProblem::NotAKeyExpression`]) carry
//! Zenoh's own message and are named so that is visible at the use site.

use crate::identity::{ExecutionId, IdentityError, ProducerId};

use crate::bus::abi::{CodecError, EncodingError};
use crate::bus::topic::WildcardPublish;

/// A bus-layer error.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// A key this crate composed - the execution root, a topic key, a
    /// participant label that becomes a key segment - is not a legal, concrete
    /// Zenoh key.
    #[error("invalid bus key '{key}': {problem}")]
    InvalidKey {
        /// The offending text.
        key: String,
        /// Why it is not usable as a key.
        problem: KeyProblem,
    },

    /// A second timeline authority was requested in one process.
    ///
    /// Exactly one participant may own a timeline's coordinate, so the second
    /// request is refused rather than silently sharing authority.
    #[error(
        "a second timeline authority was requested; exactly one participant may own a timeline"
    )]
    DuplicateTimelineAuthority,

    /// This session's producer ran out of sequence numbers. The allocator fails
    /// closed rather than wrapping to zero, which every receiver would read as
    /// a replay from the same producer.
    #[error("this session's sample sequence is exhausted")]
    SequenceExhausted,

    /// Zenoh did not honor the producer identity pinned by the unique bus
    /// owner. Continuing would make provenance and Ready attribution lie.
    #[error("bus session identity mismatch: expected producer {expected}, observed {observed}")]
    SessionIdentityMismatch {
        /// Identity minted by the owner and written to the config.
        expected: ProducerId,
        /// Identity read back from the opened Zenoh session.
        observed: ProducerId,
    },

    /// Zenoh did not honor the execution identity pinned by a router.
    /// Continuing would let one router advertise and route a different run
    /// than the one the supervisor selected.
    #[error("router execution identity mismatch: expected {expected}, observed {observed}")]
    ExecutionIdentityMismatch {
        /// Execution identity requested in the router configuration.
        expected: ExecutionId,
        /// Execution identity read back from the opened Zenoh session.
        observed: ExecutionId,
    },

    /// A codec failure encoding or decoding a body.
    #[error(transparent)]
    Codec(#[from] CodecError),

    /// An inbound sample carried a codec id this wire ABI does not implement.
    #[error("unsupported codec id {codec} on '{topic}'")]
    UnsupportedCodec {
        /// The id the wire carried. Deliberately a raw `u8`: it is precisely
        /// the ids that are *not* a [`CodecId`](crate::bus::abi::CodecId) that reach
        /// here.
        codec: u8,
        /// The family-rooted topic key it arrived on.
        topic: String,
    },

    /// A sample's envelope - the encoding string or the metadata attachment -
    /// was missing or malformed.
    #[error("invalid bus metadata on '{topic}': {problem}")]
    Metadata {
        /// The family-rooted topic key.
        topic: String,
        /// Which part of the envelope was wrong.
        problem: MetadataProblem,
    },

    /// An ordered outbound lane was saturated; the value was refused rather
    /// than blocking the step loop. State and setpoint replacement do not use
    /// this error while their new value fits the global byte bound.
    #[error("outbound {bound} bound on '{topic}'; value was not accepted")]
    Saturated {
        /// The family-rooted topic key.
        topic: String,
        /// Which of the queue's two bounds was hit.
        bound: OutboundBound,
    },

    /// A stream publisher would have to block to preserve its ordered bounded
    /// queue. The chunk was not accepted and the caller must retry or handle
    /// the loss explicitly.
    #[error("stream would block on '{topic}'")]
    WouldBlock {
        /// The family-rooted topic key.
        topic: String,
    },

    /// An ordered stream skipped one or more positions for this receiver.
    #[error(
        "stream gap on '{topic}' from producer {producer}: expected position {expected}, observed {observed}"
    )]
    StreamGap {
        topic: String,
        producer: ProducerId,
        expected: u64,
        observed: u64,
    },

    /// An ordered stream sample omitted its required per-topic position.
    #[error("stream sample on '{topic}' has no stream position")]
    MissingStreamPosition { topic: String },

    /// An ordered stream repeated or regressed a position.
    #[error(
        "stream position regressed on '{topic}' from producer {producer}: expected at least {expected}, observed {observed}"
    )]
    StreamPositionRegressed {
        topic: String,
        producer: ProducerId,
        expected: u64,
        observed: u64,
    },

    /// A stream receiver observed more producer sources than its fixed
    /// position-history bound can retain. The receiver fails closed rather
    /// than evicting an older producer's history and making future gaps
    /// ambiguous.
    #[error("stream receiver on '{topic}' exceeded its {limit}-source position-history bound")]
    TooManyStreamSources { topic: String, limit: usize },

    /// A setpoint receiver observed more producer sources than its fixed
    /// source-bound storage can retain. The receiver fails closed rather than
    /// evicting an older producer's actionable intent before authority sees it.
    #[error("setpoint receiver on '{topic}' exceeded its {limit}-source bound")]
    TooManySetpointSources { topic: String, limit: usize },

    /// A Zenoh session id is not a legal Phoxal identity. Something is
    /// listening on that endpoint, but it is not a Phoxal peer.
    #[error("session id '{zid}' is not a phoxal {role}: {source}")]
    ForeignSessionId {
        /// The session id Zenoh reported, rendered as Zenoh renders it.
        zid: String,
        /// The Phoxal identity it failed to be.
        role: SessionIdRole,
        /// Why the value is not that identity.
        source: IdentityError,
    },

    /// The underlying Zenoh transport failed.
    ///
    /// The payload is Zenoh's own message text. It is the one deliberately
    /// free-text variant: Zenoh reports transport failures as boxed opaque
    /// errors, so there is no structure to recover without inventing one.
    #[error("bus transport error: {0}")]
    Transport(String),

    /// Attempted to publish on a wildcard (subscribe-only) topic.
    #[error(transparent)]
    WildcardPublish(#[from] WildcardPublish),

    /// The subscriber's source task ended (session closed).
    #[error("subscriber closed")]
    Closed,
}

/// Why a composed key or key segment is not usable.
#[derive(Debug, thiserror::Error)]
pub enum KeyProblem {
    /// Zenoh itself rejected the composed expression. The text is Zenoh's own;
    /// its key-expression errors are boxed and carry no structure to match on.
    #[error("not a legal Zenoh key expression: {0}")]
    NotAKeyExpression(String),
    /// An empty key, or a key with an empty segment.
    #[error("must be a non-empty path with no empty segment")]
    Empty,
    /// A value that has to be exactly one concrete segment carried a separator.
    #[error("must be one concrete key segment, with no '/'")]
    NotOneSegment,
    /// A wildcard where the caller must name one concrete key. Widening an
    /// observation to a selector would silently watch keys nobody asked about.
    #[error("must be concrete: a wildcard is not a key")]
    Wildcard,
    /// A key is reserved for a narrower authority-specific declaration API.
    #[error("uses an authority-reserved key prefix")]
    ReservedPrefix,
    /// A label that would exceed the wire budget reserved for it.
    #[error("exceeds the {limit}-byte limit")]
    TooLong {
        /// The byte budget.
        limit: usize,
    },
}

/// Which part of a sample envelope was missing or malformed.
#[derive(Debug, thiserror::Error)]
pub enum MetadataProblem {
    /// A query carried no request payload at all.
    #[error("missing a request payload")]
    MissingPayload,
    /// The sample carried no Zenoh encoding string.
    #[error("missing an encoding string")]
    MissingEncoding,
    /// The encoding string is not a Phoxal encoding string.
    #[error("malformed encoding string: {0}")]
    MalformedEncoding(#[from] EncodingError),
    /// The sample carried no [`BusMetadata`](crate::bus::metadata::BusMetadata)
    /// attachment.
    #[error("missing a BusMetadata attachment")]
    MissingAttachment,
    /// The attachment bytes are not a `BusMetadata`.
    #[error("malformed BusMetadata: {0}")]
    MalformedAttachment(#[from] rmp_serde::decode::Error),
    /// The encoding string and the attachment name different codecs, so the
    /// sample does not agree with itself about how to read its own body.
    #[error(
        "encoding/BusMetadata codec mismatch: encoding codec={encoding}, metadata codec={attachment}"
    )]
    CodecMismatch {
        /// The codec the encoding string named.
        encoding: u8,
        /// The codec the attachment named.
        attachment: u8,
    },
    /// The outbound attachment could not be encoded.
    #[error("failed to encode BusMetadata: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
}

/// Which bound of the outbound queue a publish hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundBound {
    /// The ordered lane already holds its maximum number of values.
    Sample,
    /// The queue already holds its maximum number of bytes.
    Byte,
}

impl std::fmt::Display for OutboundBound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutboundBound::Sample => formatter.write_str("sample"),
            OutboundBound::Byte => formatter.write_str("byte"),
        }
    }
}

/// The Phoxal identity a Zenoh session id was read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionIdRole {
    /// A router's session id, which is the execution it routes.
    Execution,
    /// Any session's id, which is the producer identity it publishes under.
    Producer,
}

impl std::fmt::Display for SessionIdRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionIdRole::Execution => formatter.write_str("execution"),
            SessionIdRole::Producer => formatter.write_str("producer"),
        }
    }
}

impl BusError {
    /// A key Zenoh itself refused.
    pub(crate) fn not_a_key_expression(
        key: impl Into<String>,
        error: impl std::fmt::Display,
    ) -> Self {
        BusError::InvalidKey {
            key: key.into(),
            problem: KeyProblem::NotAKeyExpression(error.to_string()),
        }
    }

    /// A key this crate refused before ever handing it to Zenoh.
    pub(crate) fn invalid_key(key: impl Into<String>, problem: KeyProblem) -> Self {
        BusError::InvalidKey {
            key: key.into(),
            problem,
        }
    }

    /// A malformed sample envelope on `topic`.
    pub(crate) fn metadata(topic: impl Into<String>, problem: impl Into<MetadataProblem>) -> Self {
        BusError::Metadata {
            topic: topic.into(),
            problem: problem.into(),
        }
    }
}

/// Bus result alias.
pub type Result<T> = std::result::Result<T, BusError>;
