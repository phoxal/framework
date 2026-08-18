//! The query error wire shape.
//!
//! A query success reply is the plain `Resp` body. A handler `Err` rides
//! Zenoh's native `ReplyError`, carrying a [`QueryFailure`]
//! (MessagePack-encoded). The caller sees a [`QueryError`]; it has no version
//! variant, because a query only ever reaches a handler on its own
//! family-rooted topic key, so a version disagreement cannot reach the
//! reply path at all.

use serde::{Deserialize, Serialize};

use crate::abi::truncate_utf8;

const MAX_QUERY_FAILURE_BYTES: usize = 64 * 1024;
const MAX_QUERY_MESSAGE_BYTES: usize = 60 * 1024;

/// The small, fixed set of handler error codes.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize,
)]
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

/// A structured handler failure carried on the error reply leg.
///
/// This envelope belongs to the frozen bootstrap-reachable subset: it is the
/// only body on the error leg of a query, so a client whose attachment
/// bootstrap is refused decodes this before it can report why, and its field
/// names, code spellings and presence rules are preserved across framework
/// majors. A change here is a bootstrap-breaking event - see `xtask/README.md`
/// "When a gate fails", rule 3 "A frozen bootstrap fact drifted".
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    ///
    /// Fallible rather than infallible: every field here is handler-supplied,
    /// so "this can never fail" is a claim about data this type does not own.
    /// This runs on the error-reply path, and panicking while reporting an
    /// error would replace a failed query with a failed process.
    pub fn encode(&self) -> std::result::Result<Vec<u8>, rmp_serde::encode::Error> {
        let encoded = rmp_serde::to_vec_named(self)?;
        if encoded.len() <= MAX_QUERY_FAILURE_BYTES {
            return Ok(encoded);
        }

        let mut bounded = self.clone();
        bounded.details = None;
        bounded.details_encoding = None;
        bounded.message = truncate_utf8(&bounded.message, MAX_QUERY_MESSAGE_BYTES);
        let encoded = rmp_serde::to_vec_named(&bounded)?;
        debug_assert!(encoded.len() <= MAX_QUERY_FAILURE_BYTES);
        Ok(encoded)
    }

    /// Decode from the MessagePack error-reply payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        if bytes.len() > MAX_QUERY_FAILURE_BYTES {
            return Err(rmp_serde::decode::Error::Syntax(format!(
                "QueryFailure exceeds the {MAX_QUERY_FAILURE_BYTES}-byte limit"
            )));
        }
        rmp_serde::from_slice(bytes)
    }
}

/// What a `Querier` returns to the caller. There is no version variant: a query
/// only ever reaches a handler on its own family-rooted topic key, so a
/// version disagreement never reaches the reply path.
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

/// What a participant query handler returns: `Ok(response)` or a structured
/// [`QueryFailure`].
pub type QueryResult<T> = std::result::Result<T, QueryFailure>;

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(failure: &QueryFailure) -> Vec<u8> {
        failure.encode().expect("a test failure encodes")
    }

    /// The error leg's field names and code spellings are written out, because
    /// a client refused at the attachment bootstrap reads exactly these off the
    /// wire to say what happened.
    ///
    /// This fact is part of the frozen bootstrap-reachable subset and is
    /// preserved across framework majors. A change here is a bootstrap-breaking
    /// event - see `xtask/README.md` "When a gate fails", rule 3 "A frozen
    /// bootstrap fact drifted".
    #[test]
    fn the_bootstrap_error_leg_is_pinned_to_its_literal_fields() {
        let failure = QueryFailure::not_found("no such entity");
        assert_eq!(
            serde_json::to_value(&failure).expect("a failure serializes"),
            serde_json::json!({"code": "not_found", "message": "no such entity"}),
            "an absent detail is absent on the wire, not a null"
        );

        let mut detailed = QueryFailure::internal("with detail");
        detailed.details = Some(vec![1]);
        detailed.details_encoding = Some("application/phoxal-test".to_string());
        assert_eq!(
            serde_json::to_value(&detailed)
                .expect("a detailed failure serializes")
                .as_object()
                .expect("a failure is a map")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["code", "details", "details_encoding", "message"]
        );

        for (code, spelling) in [
            (QueryCode::NotFound, "not_found"),
            (QueryCode::InvalidArgument, "invalid_argument"),
            (QueryCode::Internal, "internal"),
            (QueryCode::Unavailable, "unavailable"),
            (QueryCode::Unimplemented, "unimplemented"),
            (QueryCode::DeadlineExceeded, "deadline_exceeded"),
        ] {
            assert_eq!(
                serde_json::to_value(code).expect("a code serializes"),
                serde_json::Value::String(spelling.to_owned())
            );
        }
    }

    #[test]
    fn every_query_code_round_trips() {
        let codes = [
            QueryCode::NotFound,
            QueryCode::InvalidArgument,
            QueryCode::Internal,
            QueryCode::Unavailable,
            QueryCode::Unimplemented,
            QueryCode::DeadlineExceeded,
        ];
        for code in codes {
            let failure = QueryFailure::new(code, format!("{code:?}"));
            assert_eq!(QueryFailure::decode(&encoded(&failure)).unwrap(), failure);
        }
    }

    #[test]
    fn query_failure_details_round_trip() {
        let mut failure = QueryFailure::internal("extra detail");
        failure.details = Some(vec![1, 2, 3, 4]);
        failure.details_encoding = Some("application/phoxal-test".to_string());

        assert_eq!(QueryFailure::decode(&encoded(&failure)).unwrap(), failure);
    }

    /// The error reply is a bounded wire value at both ends. An oversized
    /// failure sheds its optional payload and truncates its message rather than
    /// putting an unbounded value on the wire, and the decoder refuses one it
    /// would have to allocate for.
    #[test]
    fn an_error_reply_stays_inside_its_wire_limit_in_both_directions() {
        let mut failure = QueryFailure::internal("\u{e9}".repeat(100_000));
        failure.details = Some(vec![7; 100_000]);
        failure.details_encoding = Some("x".repeat(100_000));

        let bytes = encoded(&failure);
        assert!(bytes.len() <= MAX_QUERY_FAILURE_BYTES);
        let decoded = QueryFailure::decode(&bytes).expect("bounded failure decodes");
        assert_eq!(decoded.code, QueryCode::Internal);
        assert!(decoded.details.is_none());
        assert!(decoded.details_encoding.is_none());

        let error = QueryFailure::decode(&vec![0_u8; MAX_QUERY_FAILURE_BYTES + 1]).unwrap_err();
        assert!(error.to_string().contains("65536-byte limit"));
    }
}
