//! The supervisor's retained view of the live `runtime/telemetry` stream.
//!
//! Retention depth and eviction are the supervisor's decisions; this fragment owns
//! the paged request and the record shape a consumer reads back. A record is a
//! retained [`Rollup`](crate::api::runtime::telemetry::Rollup) with the
//! attribution retention has to store explicitly, because the bus envelope that
//! carried it is gone by the time the view is served.

pub use crate::api::runtime::telemetry::{BufferKind, Cursor, Direction, Rollup, Step, Topic};

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

/// One retained rollup, plus the producer identity and truncation count the
/// live envelope carried.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Record {
    pub sequence: u64,
    pub participant_id: String,
    pub truncated: u32,
    pub window_ns: u64,
    pub step: Option<Step>,
    pub topics: Vec<Topic>,
    pub overflow: Option<Topic>,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Snapshot {
    pub cursor: Cursor,
    pub records: Vec<Record>,
    /// Records evicted to stay inside the retention budget.
    pub capacity_evictions: u64,
    /// The cursor for the next older page, absent at the end of the window.
    pub next_before_sequence: Option<u64>,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Follow {
    pub cursor: Cursor,
    pub record: Record,
}

phoxal_macros::protocol_fragment! {
    path supervisor / telemetry;

    snapshot: Query<SnapshotRequest, Snapshot>;
    follow: Stream<Follow, Out>;
}
