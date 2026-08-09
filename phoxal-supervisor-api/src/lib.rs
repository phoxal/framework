//! Supervisor-owned process protocol.
//!
//! This surface is intentionally independent of the robot API revision. A
//! process can therefore report an unknown-but-valid `RobotApiVersion` without
//! requiring a robot-domain compatibility adapter.

use phoxal_macros::phoxal_api_tree;
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

phoxal_api_tree! {
    protocol supervisor {
        logs(participant_id) {
            struct Timestamp { unix_seconds: i64, nanos: u32 }
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum Level { Error, Warn, Info, Debug, Trace }
            #[serde(untagged)]
            enum LogValue {
                Bool(bool),
                I64(i64),
                U64(u64),
                F64(#[serde(deserialize_with = "crate::deserialize_finite_f64")] f64),
                String(String),
            }
            struct Event {
                seq: u64,
                time: Timestamp,
                level: Level,
                target: String,
                message: String,
                fields: ::std::collections::BTreeMap<String, LogValue>,
                dropped: u32,
                #[serde(default)]
                truncated: u32,
            }
            topic self: diagnostic Event delivery stream;
        }

        runtime {
            #[derive(Eq)]
            struct Cursor {
                sequence: u64,
            }

            #[derive(Copy, Eq, Ord, PartialOrd)]
            #[serde(rename_all = "snake_case")]
            enum Direction { Publish, Subscribe, Mixed }

            #[derive(Copy, Eq, Ord, PartialOrd)]
            #[serde(rename_all = "snake_case")]
            enum BufferKind { Outbound, Latest, Subscriber, Mixed }

            struct Step {
                target_period_ns: u64,
                completed: u64,
                errors: u64,
                mean_duration_ns: u64,
                max_duration_ns: u64,
                mean_lateness_ns: u64,
                max_lateness_ns: u64,
                missed_ticks: u64,
                overruns: u64,
            }

            struct Topic {
                topic: String,
                direction: Direction,
                buffer_kind: BufferKind,
                count: u64,
                rate_millihz: u64,
                drops: u64,
                latest_overwrites: u64,
                bounded_evictions: u64,
                capacity: u64,
                current_depth: u64,
                high_water_depth: u64,
                decode_errors: u64,
                timeline_filtered: u64,
                overflowed_rows: u32,
            }
        }

        telemetry {
            struct Rollup {
                window_ns: u64,
                step: Option<crate::supervisor::runtime::Step>,
                topics: Vec<crate::supervisor::runtime::Topic>,
                overflow: Option<crate::supervisor::runtime::Topic>,
            }
            struct SnapshotRequest {
                participant_id: Option<String>,
                limit: u32,
                before_sequence: Option<u64>,
            }
            struct Record {
                sequence: u64,
                participant_id: String,
                truncated: u32,
                window_ns: u64,
                step: Option<crate::supervisor::runtime::Step>,
                topics: Vec<crate::supervisor::runtime::Topic>,
                overflow: Option<crate::supervisor::runtime::Topic>,
            }
            struct Snapshot {
                cursor: crate::supervisor::runtime::Cursor,
                records: Vec<Record>,
                capacity_evictions: u64,
                next_before_sequence: Option<u64>,
            }
            struct Follow {
                cursor: crate::supervisor::runtime::Cursor,
                record: Record,
            }
            topic rollup: diagnostic Rollup;
            topic snapshot: query SnapshotRequest => Snapshot;
            topic follow: diagnostic Follow;
        }

        log {
            struct SnapshotRequest {}
            struct Timestamp { unix_seconds: i64, nanos: u32 }
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum Level { Error, Warn, Info, Debug, Trace }
            #[serde(untagged)]
            enum LogValue { Bool(bool), I64(i64), U64(u64), F64(f64), String(String) }
            struct Record {
                sequence: u64,
                participant_id: String,
                source_sequence: u64,
                time: Timestamp,
                level: Level,
                target: String,
                message: String,
                fields: ::std::collections::BTreeMap<String, LogValue>,
                dropped: u32,
                truncated: u32,
            }
            struct Snapshot {
                cursor: crate::supervisor::runtime::Cursor,
                ingest_dropped: u64,
                records: Vec<Record>,
            }
            struct Follow {
                cursor: crate::supervisor::runtime::Cursor,
                ingest_dropped: u64,
                record: Record,
            }
            topic snapshot: query SnapshotRequest => Snapshot;
            topic follow: diagnostic Follow;
        }

        asset {
            struct GetRequest { path: String }
            enum GetResponse { Found { bytes: Vec<u8> }, Missing, InvalidPath }
            topic get: query GetRequest => GetResponse;
        }
    }
}

#[cfg(test)]
mod tests {
    use phoxal_bus::ContractBody;

    use crate::supervisor;

    #[test]
    fn process_topics_are_protocol_qualified_not_robot_api_qualified() {
        assert_eq!(
            <supervisor::telemetry::Rollup as ContractBody>::TOPIC,
            "supervisor/telemetry/rollup"
        );
        assert_eq!(
            <supervisor::asset::GetRequest as ContractBody>::TOPIC,
            "supervisor/asset/get"
        );
    }

    #[test]
    fn log_values_reject_non_finite_floats_on_decode() {
        let encoded = rmp_serde::to_vec_named(&supervisor::logs::LogValue::F64(f64::NAN))
            .expect("messagepack permits a non-finite test input");
        assert!(rmp_serde::from_slice::<supervisor::logs::LogValue>(&encoded).is_err());
    }
}
