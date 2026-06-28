//! Query error wire shape (D31/D43f).
//!
//! A query success reply is the plain `Resp` body (D62). A handler `Err` rides
//! Zenoh's native `ReplyError`, carrying a [`QueryFailure`] (MessagePack-encoded).
//! Both are part of `bus_abi` and golden-tested. The caller sees a
//! [`QueryError`]; there is no `Version` variant — one graph runs one
//! `api_version` (D63).

use serde::{Deserialize, Serialize};

/// The small, fixed set of handler error codes (D31).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryCode {
    /// The requested entity does not exist.
    NotFound,
    /// The request was malformed or semantically invalid.
    InvalidArgument,
    /// An unexpected server-side failure.
    Internal,
    /// The server is temporarily unable to serve.
    Unavailable,
    /// The operation is not implemented.
    Unimplemented,
    /// The server could not produce a reply in time.
    DeadlineExceeded,
}

/// A structured handler failure carried on the error reply leg (D31).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryFailure {
    /// The fixed error code.
    pub code: QueryCode,
    /// A human-readable message.
    pub message: String,
    /// Optional opaque details payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<u8>>,
    /// The encoding of `details`, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_encoding: Option<String>,
}

impl QueryFailure {
    /// A failure with `code` and `message`.
    pub fn new(code: QueryCode, message: impl Into<String>) -> Self {
        QueryFailure {
            code,
            message: message.into(),
            details: None,
            details_encoding: None,
        }
    }

    /// `NotFound`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(QueryCode::NotFound, message)
    }
    /// `InvalidArgument`.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(QueryCode::InvalidArgument, message)
    }
    /// `Internal`.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(QueryCode::Internal, message)
    }
    /// `Unavailable`.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(QueryCode::Unavailable, message)
    }
    /// `Unimplemented`.
    pub fn unimplemented(message: impl Into<String>) -> Self {
        Self::new(QueryCode::Unimplemented, message)
    }
    /// `DeadlineExceeded`.
    pub fn deadline_exceeded(message: impl Into<String>) -> Self {
        Self::new(QueryCode::DeadlineExceeded, message)
    }

    /// Encode to the MessagePack error-reply payload.
    pub fn encode(&self) -> Vec<u8> {
        rmp_serde::to_vec_named(self).expect("QueryFailure is always serializable")
    }

    /// Decode from the MessagePack error-reply payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(bytes)
    }
}

/// What a `Querier` returns to the caller (D31). No `Version` variant — one graph
/// runs one `api_version` (D63).
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// No responder answered the query.
    #[error("no responder is available for this query topic")]
    Unavailable,
    /// The query exceeded the caller-side timeout.
    #[error("query timed out: {0:?}")]
    Timeout(QueryFailure),
    /// The handler returned a structured failure.
    #[error("query server error: {0:?}")]
    Server(QueryFailure),
    /// The response body could not be decoded.
    #[error("failed to decode query response: {0}")]
    Decode(String),
    /// A protocol-level error (bad metadata, encode failure, transport).
    #[error("query protocol error: {0}")]
    Protocol(String),
    /// More than one responder answered an exclusive query topic.
    #[error("multiple responders answered an exclusive query topic")]
    TooManyResponders,
}

/// What a `#[server]`/`#[server_snapshot]` handler returns (D43f): `Ok(resp)` or a
/// structured [`QueryFailure`].
pub type ServerResult<T> = std::result::Result<T, QueryFailure>;
