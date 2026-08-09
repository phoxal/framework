//! Supervisor-owned process protocol.
//!
//! This surface is intentionally independent of the robot API revision. A
//! process can therefore report an unknown-but-valid `RobotApiVersion` without
//! requiring a robot-domain compatibility adapter.

use phoxal_macros::phoxal_protocol;
use serde::Deserialize;

pub(crate) fn deserialize_finite_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "expected a finite floating-point value",
        ))
    }
}

/// Ordinary protocol payloads. The generated `supervisor` module below owns
/// endpoint descriptors and topic builders; these modules own only wire data.
pub mod payload {
    pub mod logs {
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Timestamp {
            pub unix_seconds: i64,
            pub nanos: u32,
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Level {
            Error,
            Warn,
            Info,
            Debug,
            Trace,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(untagged)]
        pub enum LogValue {
            Bool(bool),
            I64(i64),
            U64(u64),
            F64(#[serde(deserialize_with = "crate::deserialize_finite_f64")] f64),
            String(String),
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Event {
            pub seq: u64,
            pub time: Timestamp,
            pub level: Level,
            pub target: String,
            pub message: String,
            pub fields: ::std::collections::BTreeMap<String, LogValue>,
            pub dropped: u32,
            #[serde(default)]
            pub truncated: u32,
        }
    }

    pub mod runtime {
        #[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Cursor {
            pub sequence: u64,
        }

        #[derive(
            Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum Direction {
            Publish,
            Subscribe,
            Mixed,
        }

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
            pub overflowed_rows: u32,
        }
    }

    pub mod telemetry {
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Rollup {
            pub window_ns: u64,
            pub step: Option<crate::payload::runtime::Step>,
            pub topics: Vec<crate::payload::runtime::Topic>,
            pub overflow: Option<crate::payload::runtime::Topic>,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct SnapshotRequest {
            pub participant_id: Option<String>,
            pub limit: u32,
            pub before_sequence: Option<u64>,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Record {
            pub sequence: u64,
            pub participant_id: String,
            pub truncated: u32,
            pub window_ns: u64,
            pub step: Option<crate::payload::runtime::Step>,
            pub topics: Vec<crate::payload::runtime::Topic>,
            pub overflow: Option<crate::payload::runtime::Topic>,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Snapshot {
            pub cursor: crate::payload::runtime::Cursor,
            pub records: Vec<Record>,
            pub capacity_evictions: u64,
            pub next_before_sequence: Option<u64>,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Follow {
            pub cursor: crate::payload::runtime::Cursor,
            pub record: Record,
        }
    }

    pub mod log {
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct SnapshotRequest {}

        /// Logs use the same timestamp, severity and finite-value vocabulary
        /// at ingestion and when the supervisor later serves retained records.
        pub use crate::payload::logs::{Level, LogValue, Timestamp};

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Snapshot {
            pub cursor: crate::payload::runtime::Cursor,
            pub ingest_dropped: u64,
            pub records: Vec<Record>,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct Follow {
            pub cursor: crate::payload::runtime::Cursor,
            pub ingest_dropped: u64,
            pub record: Record,
        }
    }

    pub mod asset {
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct GetRequest {
            pub path: String,
        }

        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        pub enum GetResponse {
            Found { bytes: Vec<u8> },
            Missing,
            InvalidPath,
        }
    }
}

phoxal_protocol! {
    protocol supervisor {
        logs(participant_id) {
            topic self: Stream<crate::payload::logs::Event>;
        }

        runtime {
            // The runtime payloads are owned by `payload::runtime`; this node
            // contributes only endpoint descriptors and topic paths.
        }

        telemetry {
            topic rollup: Stream<crate::payload::telemetry::Rollup>;
            query snapshot: crate::payload::telemetry::SnapshotRequest => crate::payload::telemetry::Snapshot;
            topic follow: Stream<crate::payload::telemetry::Follow>;
        }

        log {
            query snapshot: crate::payload::log::SnapshotRequest => crate::payload::log::Snapshot;
            topic follow: Stream<crate::payload::log::Follow>;
        }

        asset {
            query get: crate::payload::asset::GetRequest => crate::payload::asset::GetResponse;
        }
    }
}

#[cfg(test)]
mod tests {
    use phoxal_bus::{EndpointDescriptor, EndpointKind};

    use crate::supervisor;

    #[test]
    fn process_topics_are_protocol_qualified_not_robot_api_qualified() {
        assert_eq!(
            <supervisor::endpoint::telemetry::RollupEndpoint as EndpointDescriptor>::TOPIC,
            "supervisor/telemetry/rollup"
        );
        assert_eq!(
            <supervisor::endpoint::asset::GetEndpoint as EndpointDescriptor>::TOPIC,
            "supervisor/asset/get"
        );
        assert_eq!(
            <supervisor::endpoint::telemetry::RollupEndpoint as EndpointDescriptor>::KIND,
            EndpointKind::Stream
        );
        assert_eq!(
            <supervisor::endpoint::asset::GetEndpoint as EndpointDescriptor>::KIND,
            EndpointKind::Query
        );
    }

    #[test]
    fn log_values_reject_non_finite_floats_on_decode() {
        let encoded = rmp_serde::to_vec_named(&supervisor::logs::LogValue::F64(f64::NAN))
            .expect("messagepack permits a non-finite test input");
        assert!(rmp_serde::from_slice::<supervisor::logs::LogValue>(&encoded).is_err());
        assert!(rmp_serde::from_slice::<supervisor::log::LogValue>(&encoded).is_err());
    }
}
