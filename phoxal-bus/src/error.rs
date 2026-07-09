//! Bus error type.

use crate::codec::CodecError;

/// A bus-layer error.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// The namespace / robot id / key root was not a legal, concrete key.
    #[error("invalid bus namespace: {0}")]
    Namespace(String),

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
