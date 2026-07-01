//! Server-handler plumbing the macros + runner share (D16).
//!
//! - [`Snapshot<S>`] is the committed, read-only state a `#[server_snapshot]`
//!   handler reads concurrently.
//! - [`ServerReply`]/[`ServerOutcome`] are the encoded result a generated server
//!   dispatcher returns to the runner.

use std::ops::Deref;
use std::sync::Arc;

use crate::bus::QueryFailure;

/// A committed, read-only snapshot of a participant's state (D16). Cheap to clone
/// (an `Arc`); handed to `#[server_snapshot]` handlers so they read without
/// touching `&mut self`.
pub struct Snapshot<S> {
    inner: Arc<S>,
}

impl<S> Snapshot<S> {
    /// Wrap a committed `Arc<S>`.
    pub fn from_arc(inner: Arc<S>) -> Self {
        Snapshot { inner }
    }

    /// The underlying state.
    pub fn get(&self) -> &S {
        &self.inner
    }
}

impl<S> Clone for Snapshot<S> {
    fn clone(&self) -> Self {
        Snapshot {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S> Deref for Snapshot<S> {
    type Target = S;
    fn deref(&self) -> &S {
        &self.inner
    }
}

/// A successful server reply: the encoded plain `Resp` body plus the metadata the
/// runner needs to stamp the reply (D62).
#[derive(Debug)]
pub struct ServerReply {
    /// MessagePack-encoded `Resp` body.
    pub payload: Vec<u8>,
    /// `<Resp as ContractBody>::FAMILY`.
    pub family: &'static str,
    /// `<Resp::Api as ApiVersion>::ID`.
    pub api_version: &'static str,
    /// `<Resp as ContractBody>::SCHEMA_ID`.
    pub schema_id: &'static str,
}

/// What a generated server dispatcher returns: a [`ServerReply`] or a structured
/// [`QueryFailure`].
pub type ServerOutcome = std::result::Result<ServerReply, QueryFailure>;
