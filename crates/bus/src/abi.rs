//! The wire ABI: the Zenoh encoding string, the codec id it names, and the body
//! codec that id selects.
//!
//! A version-qualified Zenoh key carries wire identity
//! (`<Endpoint as EndpointDescriptor>::TOPIC`, e.g. `v0.1/drive/target`), while the
//! encoding string records the codec. A receiver therefore sees only samples
//! for its subscribed contract key and validates their encoding.
//!
//! The body is always the plain payload: the codec never adds a version
//! envelope, because the version is already folded into the key.
//!
//! The identity of this ABI as a whole is
//! [`phoxal_runtime_contract::version::BusAbi`], declared in every
//! participant's embedded metadata record. The `phoxal/v0` prefix below is
//! unrelated: it is per-sample wire overhead and stays short.

use std::str::FromStr;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Hard ceiling for one decoded endpoint payload. This matches the session
/// outbound byte budget, so a peer cannot ask a consumer to deserialize a
/// body larger than any conforming local publisher may queue.
pub const MAX_DECODE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The fixed prefix every Phoxal encoding string opens with.
const ENCODING_PREFIX: &str = "phoxal/v0";

/// The wire codec identifier carried in bus metadata. One codec in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CodecId {
    /// MessagePack with named fields (`rmp_serde`).
    MessagePack = 1,
}

impl CodecId {
    /// The numeric id carried on the wire.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Resolve a wire codec id, rejecting anything unknown before body decode.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(CodecId::MessagePack),
            _ => None,
        }
    }

    /// The Zenoh encoding string naming this codec: `phoxal/v0;codec=<id>`.
    pub fn encoding_string(self) -> String {
        format!("{ENCODING_PREFIX};codec={}", self.as_u8())
    }
}

/// The parsed Zenoh encoding string: just the codec, because contract identity
/// lives in the key.
///
/// The codec is kept as the raw `u8` the wire carried rather than a
/// [`CodecId`]: an id this ABI does not know is still a well-formed encoding
/// string, and the receiver has to be able to report *which* unknown id it was
/// rejected for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingMetadata {
    /// Numeric codec id.
    pub codec: u8,
}

impl EncodingMetadata {
    /// The codec id, if recognized by this wire ABI.
    pub fn codec_id(&self) -> Option<CodecId> {
        CodecId::from_u8(self.codec)
    }
}

impl FromStr for EncodingMetadata {
    type Err = EncodingError;

    /// Parse and validate the Zenoh encoding string. Format:
    /// `phoxal/v0;codec=<id>`.
    fn from_str(value: &str) -> Result<Self, EncodingError> {
        let mut parts = value.split(';');
        // `split` always yields at least one item, so the prefix is the head of
        // the string whether or not any field follows it.
        let prefix = parts.next().unwrap_or_default();
        if prefix != ENCODING_PREFIX {
            return Err(EncodingError::Prefix {
                found: prefix.to_string(),
            });
        }

        let mut codec = None;
        for field in parts {
            let (key, value) =
                field
                    .split_once('=')
                    .ok_or_else(|| EncodingError::MissingAssignment {
                        field: field.to_string(),
                    })?;
            if value.is_empty() {
                return Err(EncodingError::EmptyField {
                    field: key.to_string(),
                });
            }
            match key {
                "codec" => {
                    let parsed = value.parse::<u8>().map_err(|_| EncodingError::NotAU8 {
                        field: key.to_string(),
                        value: value.to_string(),
                    })?;
                    if codec.replace(parsed).is_some() {
                        return Err(EncodingError::DuplicateField {
                            field: key.to_string(),
                        });
                    }
                }
                _ => {
                    return Err(EncodingError::UnknownField {
                        field: key.to_string(),
                    });
                }
            }
        }

        Ok(EncodingMetadata {
            codec: codec.ok_or(EncodingError::MissingCodec)?,
        })
    }
}

/// Why an inbound Zenoh encoding string is not a Phoxal encoding string.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EncodingError {
    /// The string does not open with the Phoxal encoding prefix.
    #[error("expected encoding prefix '{ENCODING_PREFIX}', got '{found}'")]
    Prefix {
        /// The prefix that was there instead.
        found: String,
    },
    /// A `;`-separated field is not a `key=value` pair.
    #[error("encoding field '{field}' is missing '='")]
    MissingAssignment {
        /// The offending field, verbatim.
        field: String,
    },
    /// A field carried an empty value.
    #[error("encoding field '{field}' is empty")]
    EmptyField {
        /// The field name.
        field: String,
    },
    /// The codec field is not a wire codec id.
    #[error("encoding field '{field}' is not a u8: '{value}'")]
    NotAU8 {
        /// The field name.
        field: String,
        /// The value that failed to parse.
        value: String,
    },
    /// The same field appeared twice, so the string has no single meaning.
    #[error("duplicate encoding field '{field}'")]
    DuplicateField {
        /// The field name.
        field: String,
    },
    /// A field this ABI does not define. Unknown fields are rejected rather
    /// than ignored: a sender that added one is speaking a wire ABI this
    /// receiver does not implement.
    #[error("unknown encoding field '{field}'")]
    UnknownField {
        /// The field name.
        field: String,
    },
    /// The codec field is absent, so the payload cannot be decoded at all.
    #[error("encoding string is missing codec")]
    MissingCodec,
}

/// Truncate `value` to at most `max_bytes`, cutting at a char boundary.
///
/// Wire values that carry caller-supplied text - a participant label, a handler
/// error message - are bounded here rather than rejected: a sample or an error
/// reply is still worth delivering with a shortened label, and dropping it
/// outright would lose the report along with the overlong field.
pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// A wire codec for contract bodies. The body is always the plain payload - the
/// codec never adds a version envelope.
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
///
/// Both variants carry the codec library's own message text. It is opaque by
/// construction - the codec is generic over every endpoint payload, so there is no
/// structure this crate could recover from a serde failure without teaching the
/// bus about the shape of every body it carries.
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
    fn an_encoding_string_carries_only_the_codec() {
        let encoding = CodecId::MessagePack.encoding_string();
        assert_eq!(encoding, "phoxal/v0;codec=1");
        let parsed: EncodingMetadata = encoding.parse().expect("the codec string parses");
        assert_eq!(parsed.codec_id(), Some(CodecId::MessagePack));
    }

    #[test]
    fn an_unknown_codec_id_parses_but_does_not_resolve() {
        // A well-formed string naming a codec this ABI does not implement must
        // survive parsing, so the receiver can name the id it rejected.
        let parsed: EncodingMetadata = "phoxal/v0;codec=99".parse().expect("well-formed");
        assert_eq!(parsed.codec, 99);
        assert_eq!(parsed.codec_id(), None);
    }

    #[test]
    fn a_malformed_encoding_string_names_what_was_wrong() {
        assert_eq!(
            "other/v0;codec=1".parse::<EncodingMetadata>(),
            Err(EncodingError::Prefix {
                found: "other/v0".to_string()
            })
        );
        assert_eq!(
            "".parse::<EncodingMetadata>(),
            Err(EncodingError::Prefix {
                found: String::new()
            })
        );
        assert_eq!(
            "phoxal/v0".parse::<EncodingMetadata>(),
            Err(EncodingError::MissingCodec)
        );
        assert_eq!(
            "phoxal/v0;codec".parse::<EncodingMetadata>(),
            Err(EncodingError::MissingAssignment {
                field: "codec".to_string()
            })
        );
        assert_eq!(
            "phoxal/v0;codec=".parse::<EncodingMetadata>(),
            Err(EncodingError::EmptyField {
                field: "codec".to_string()
            })
        );
        assert_eq!(
            "phoxal/v0;codec=x".parse::<EncodingMetadata>(),
            Err(EncodingError::NotAU8 {
                field: "codec".to_string(),
                value: "x".to_string()
            })
        );
        assert_eq!(
            "phoxal/v0;codec=1;codec=1".parse::<EncodingMetadata>(),
            Err(EncodingError::DuplicateField {
                field: "codec".to_string()
            })
        );
        assert_eq!(
            "phoxal/v0;codec=1;schema=7".parse::<EncodingMetadata>(),
            Err(EncodingError::UnknownField {
                field: "schema".to_string()
            })
        );
    }

    #[test]
    fn messagepack_rejects_oversized_bodies_before_deserializing() {
        let bytes = vec![0_u8; MAX_DECODE_BODY_BYTES + 1];
        let error = MessagePack::decode::<Vec<u8>>(&bytes).expect_err("oversized body must fail");
        assert!(error.to_string().contains("maximum"));
    }
}
