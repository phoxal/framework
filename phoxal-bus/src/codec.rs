//! The body codec boundary (D62). One codec in v1: MessagePack with named fields.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::abi::CodecId;

/// Hard ceiling for one decoded contract body. This matches the session
/// outbound byte budget, so a peer cannot ask a consumer to deserialize a
/// body larger than any conforming local publisher may queue.
pub const MAX_DECODE_BODY_BYTES: usize = 16 * 1024 * 1024;

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
        if bytes.len() > MAX_DECODE_BODY_BYTES {
            return Err(CodecError::Decode(format!(
                "body is {} bytes; maximum is {MAX_DECODE_BODY_BYTES}",
                bytes.len()
            )));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messagepack_rejects_oversized_bodies_before_deserializing() {
        let bytes = vec![0_u8; MAX_DECODE_BODY_BYTES + 1];
        let error = MessagePack::decode::<Vec<u8>>(&bytes).expect_err("oversized body must fail");
        assert!(error.to_string().contains("maximum"));
    }
}
