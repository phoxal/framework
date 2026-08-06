//! Bus error type.

use crate::codec::CodecError;

/// A bus-layer error.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// The participant id or the composed key root was not a legal, concrete
    /// key.
    #[error("invalid bus key: {0}")]
    Namespace(String),

    /// This session's producer ran out of sequence numbers. The allocator fails
    /// closed rather than wrapping to zero, which every receiver would read as
    /// a replay from the same producer.
    #[error("this session's sample sequence is exhausted")]
    SequenceExhausted,

    /// A codec failure encoding or decoding a body.
    #[error(transparent)]
    Codec(#[from] CodecError),

    /// An inbound sample carried an unsupported codec id.
    #[error("unsupported codec id {0} on '{1}'")]
    UnsupportedCodec(u8, String),

    /// A required attachment / metadata was missing or malformed.
    #[error("invalid bus metadata on '{topic}': {detail}")]
    Metadata {
        /// The topic key.
        topic: String,
        /// What was wrong.
        detail: String,
    },

    /// The outbound queue was saturated (samples or bytes); the sample was
    /// dropped rather than blocking the step loop (D35/D43e).
    #[error("outbound queue saturated on '{topic}' ({detail}); sample dropped")]
    Saturated {
        /// The topic key.
        topic: String,
        /// Which bound was hit.
        detail: String,
    },

    /// The underlying Zenoh transport failed.
    #[error("bus transport error: {0}")]
    Transport(String),

    /// Attempted to publish on a wildcard (subscribe-only) topic.
    #[error(transparent)]
    WildcardPublish(#[from] crate::topic::WildcardPublish),

    /// The subscriber's source task ended (session closed).
    #[error("subscriber closed")]
    Closed,
}

/// Bus result alias.
pub type Result<T> = std::result::Result<T, BusError>;
