//! Read access to the runtime bundle the supervisor is running.
//!
//! The supervisor is the only process that knows where the bundle lives, so a
//! client asks it for a path rather than reaching into a filesystem it does not
//! own. What paths resolve, and which of them are refused, is `phoxald`'s
//! decision; this fragment owns only the request and the three answers.

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct GetRequest {
    pub path: String,
}

/// A missing entry and a path the supervisor refuses to resolve are distinct
/// answers, so a client can tell "not in this bundle" from "never ask that".
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum GetResponse {
    Found { bytes: Vec<u8> },
    Missing,
    InvalidPath,
}

phoxal_macros::protocol_fragment! {
    path supervisor / bundle;

    get: Query<GetRequest, GetResponse>;
}
