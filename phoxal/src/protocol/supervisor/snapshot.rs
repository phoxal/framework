//! The supervisor's complete projection of one execution, and the baseline a
//! late joiner needs before it applies updates.
//!
//! The supervisor decides what the projection contains and when it changes;
//! this module owns only its wire shape. Every published value is a complete
//! replacement, so a consumer never reconstructs state from a diff it may have
//! missed.

use crate::protocol::supervisor::execution::SnapshotDocument;

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct CurrentRequest {}

/// Query this once before subscribing so a late joiner has a baseline; then
/// apply every value received from the snapshot stream.
pub type Current = SnapshotDocument;
/// A complete replacement snapshot published after the baseline.
pub type Update = SnapshotDocument;
