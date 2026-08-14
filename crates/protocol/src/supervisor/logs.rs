//! The supervisor's retained view of the live `runtime/logs` stream.
//!
//! Retention depth, eviction, and what "recent" means are the supervisor's
//! decisions; this module owns the paged request and the record shape a
//! consumer reads back. Records use the same timestamp, severity, and
//! finite-value vocabulary as ingestion, so a retained record and a live event
//! never disagree about what a field means.

use crate::runtime::logs::{Level, LogValue, Timestamp};
use crate::runtime::telemetry::Cursor;

/// One page of retained records, newest first.
///
/// `before_sequence` is the previous page's `next_before_sequence`, so paging
/// walks backwards through a window that keeps moving without ever repeating or
/// skipping a record.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct SnapshotRequest {
    pub participant_id: Option<String>,
    pub limit: u32,
    pub before_sequence: Option<u64>,
}

/// One retained log record. The supervisor's `sequence` orders the retained
/// view; `source_sequence` is the producer's own counter, so per-producer loss
/// stays visible after retention has merged several producers.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Record {
    pub sequence: u64,
    pub participant_id: String,
    pub source_sequence: u64,
    pub time: Timestamp,
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: ::std::collections::BTreeMap<String, LogValue>,
    pub dropped: u32,
    pub truncated: u32,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Snapshot {
    pub cursor: Cursor,
    /// Records the supervisor lost at ingestion, which no page can show.
    pub ingest_dropped: u64,
    pub records: Vec<Record>,
    /// The cursor for the next older page, absent at the end of the window.
    pub next_before_sequence: Option<u64>,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Follow {
    pub cursor: Cursor,
    pub ingest_dropped: u64,
    pub record: Record,
}
