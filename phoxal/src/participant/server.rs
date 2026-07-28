//! Encoded query-handler plumbing shared by typed registrations and the runner.

use crate::bus::QueryFailure;

/// A successful server reply: the encoded plain `Resp` body (D62/D1). Identity
/// no longer rides the reply - the request already arrived on `Resp`'s own
/// version-qualified topic key, so the reply needs no separate stamp.
#[derive(Debug)]
pub struct ServerReply {
    /// MessagePack-encoded `Resp` body.
    pub payload: Vec<u8>,
}

/// What a generated server dispatcher returns: a [`ServerReply`] or a structured
/// [`QueryFailure`].
pub type ServerOutcome = std::result::Result<ServerReply, QueryFailure>;
