//! Runtime performance a process measures about itself: its step cadence and
//! its per-topic bus accounting, rolled up over one window.
//!
//! The producing participant is the bus envelope's source attribution, so a
//! rollup carries no identity field of its own. Retained, replayable views over
//! these same samples belong to whatever host tool collects them; this family
//! owns only the live rollup and the sample vocabulary it is written in.

/// Position in one retained sample sequence.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    pub sequence: u64,
}

/// Which side of a topic a row accounts for. `Mixed` exists for a summary row
/// that spans several topics and therefore names no single direction.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Publish,
    Subscribe,
    Mixed,
}

/// Which buffer a row accounts for. `Mixed` exists for a summary row that
/// spans several buffers.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BufferKind {
    Outbound,
    Latest,
    Subscriber,
    Mixed,
}

/// Step cadence measured over one window, for a participant that has a
/// cadence at all.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub target_period_ns: u64,
    pub completed: u64,
    pub errors: u64,
    pub mean_duration_ns: u64,
    pub max_duration_ns: u64,
    pub mean_lateness_ns: u64,
    pub max_lateness_ns: u64,
    pub missed_ticks: u64,
    pub overruns: u64,
}

/// One topic's traffic and buffer accounting over one window.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Topic {
    pub topic: String,
    pub direction: Direction,
    pub buffer_kind: BufferKind,
    pub count: u64,
    pub rate_millihz: u64,
    pub drops: u64,
    pub latest_overwrites: u64,
    pub bounded_evictions: u64,
    pub capacity: u64,
    pub current_depth: u64,
    pub high_water_depth: u64,
    pub decode_errors: u64,
    pub timeline_filtered: u64,
    /// Rows folded into this one; zero on an ordinary per-topic row.
    pub overflowed_rows: u32,
}

/// One window of runtime performance: the step section, the bounded per-topic
/// rows, and the single row everything that did not fit was folded into.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rollup {
    pub window_ns: u64,
    pub step: Option<Step>,
    pub topics: Vec<Topic>,
    pub overflow: Option<Topic>,
}

phoxal_macros::phoxal_api_fragment! {
    path runtime / telemetry;

    topic self: Stream<Rollup>;
}
