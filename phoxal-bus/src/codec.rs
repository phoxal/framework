//! The body codec boundary (D62). One codec in v1: MessagePack with named fields.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::abi::CodecId;

/// A wire codec for contract bodies. The body is always the plain payload - the
/// codec never adds a version envelope (D62).
pub trait Codec {
    /// The codec id carried in bus metadata.
    const ID: CodecId;

    /// Encode a body to bytes.
    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError>;

    /// Decode a body from bytes.
    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError>;
}

/// MessagePack (named fields) - the single v1 codec.
pub struct MessagePack;

impl Codec for MessagePack {
    const ID: CodecId = CodecId::MessagePack;

    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
        rmp_serde::to_vec_named(value).map_err(|e| CodecError::Encode(e.to_string()))
    }

    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
        rmp_serde::from_slice(bytes).map_err(|e| CodecError::Decode(e.to_string()))
    }
}

/// A codec encode/decode failure.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The body could not be serialized.
    #[error("failed to encode body: {0}")]
    Encode(String),
    /// The bytes could not be deserialized into the expected body.
    #[error("failed to decode body: {0}")]
    Decode(String),
}
