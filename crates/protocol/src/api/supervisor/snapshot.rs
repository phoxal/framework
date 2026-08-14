//! The supervisor's complete projection of one execution, and the baseline a
//! late joiner needs before it applies updates.
//!
//! `phoxald` decides what the projection contains and when it changes; this
//! fragment owns only its wire shape. Every published value is a complete
//! replacement, so a consumer never reconstructs state from a diff it may have
//! missed.

pub use crate::api::supervisor::execution::{
    DaemonFailure, DaemonFailureReason, DesiredState, Detail, DiagnosticText, ExitStatus,
    Lifecycle, MAX_DETAIL_BYTES, MAX_PROCESSES, MAX_STDERR_TAIL_BYTES, Process, ProcessFailure,
    ProcessFailureKind, ProcessState, Snapshot, SnapshotDocument, SnapshotError, StartupStep,
    StartupStepKind, StartupStepState, StderrTail, WallTime, WallTimeError,
};

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

phoxal_macros::protocol_fragment! {
    path supervisor / snapshot;

    self: Stream<Update, Out>;
    current: Query<CurrentRequest, Current>;
}
