//! Read access to the runtime bundle the supervisor is running.
//!
//! The supervisor is the only process that knows where the bundle lives, so a
//! client asks it for a bounded range rather than reaching into a filesystem it
//! does not own. What paths resolve, and which of them are refused, is the
//! supervisor's decision; this module owns only the request and answers.

crate::endpoints! {
    get: Query<GetRequest, GetResponse>;
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct GetRequest {
    /// The normalized bundle-relative target.
    pub path: crate::bundle::BundlePath,
    /// The first byte requested. The caller advances it by the returned byte
    /// count until the supervisor marks the final chunk.
    pub offset: u64,
}

/// A missing entry and a path the supervisor refuses to resolve are distinct
/// answers, so a client can tell "not in this bundle" from "never ask that".
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum GetResponse {
    /// One supervisor-sized range. A non-final range always makes progress.
    Chunk { bytes: Vec<u8>, eof: bool },
    /// No regular bundle entry exists at the valid requested path.
    Missing,
    /// The syntactically valid path cannot resolve to an admissible regular
    /// file under the canonical bundle root.
    InvalidPath,
    /// The target exists and is admissible, but the supervisor does not expose
    /// it to a bundle reader.
    Refused,
}
