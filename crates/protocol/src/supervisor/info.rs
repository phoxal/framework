//! What robot this supervisor is running.
//!
//! The answer is the bundle's own `manifest.json`, handed back exactly as it
//! is on disk: one schema-tagged [`phoxal_model::manifest::ManifestDocument`].
//! A client that needs the robot identity, the mounted components or a
//! runtime's configuration therefore reads the same document every participant
//! reads, instead of a second projection of it that could disagree.
//!
//! This is deliberately not part of the frozen attachment bootstrap: a client
//! asks it after `supervisor/connect` has already agreed the two trains match,
//! so the reply may be the full model rather than a version-independent
//! document.

/// Ask which robot this supervisor is running. There is nothing to select: a
/// supervisor is handed one bundle root and never reopens it.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct InfoRequest {}

/// The bundle's manifest document, byte-identical to `manifest.json`.
pub type InfoReply = phoxal_model::manifest::ManifestDocument;
