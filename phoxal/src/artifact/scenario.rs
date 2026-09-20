//! Bundle-side scenario record family.
//!
//! Owns `BundleScenarioProgram` and `BundleScenarioProducer`. The
//! marker-bearing wrapper `BundleScenarioSection` lives in
//! [`super::bundle`] alongside the rest of the bundle manifest.
//!
//! Scenario discovery, harness source generation, and case-host process
//! control remain in `cargo-phoxal`'s tool layer.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Validated scenario program identity. The supervisor rejects the
/// bundle unless every field satisfies the documented invariants;
/// see the supervisor's bundle reader for the receiver-side checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleScenarioProgram {
    /// Authored scenario name (e.g. `scenarios/ForwardTurnStop`).
    pub scenario_name: String,
    /// Bundle-relative POSIX path to the normalized program bytes.
    /// No absolute prefix, no `..` segments, no symlink escape.
    pub program_path: String,
    /// Exact byte length of the program artifact.
    pub program_byte_length: u32,
    /// Lowercase SHA-256 digest of the program bytes.
    pub program_digest: String,
    /// Fixture instance id that owns the prepared producer payloads.
    pub fixture_instance_id: String,
    /// Must be `true` for the supported scenario launch path.
    pub controlled_execution: bool,
}

/// One supervisor-owned scenario producer exposed to Runtime input admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleScenarioProducer {
    /// Owning fixture instance.
    pub instance: String,
    /// Generated public output port name.
    pub port: String,
    /// Fully-qualified Protobuf service declaring the generated output.
    pub service_fqn: String,
    /// Protobuf method declaring the generated output.
    pub method: String,
    /// Public observation semantic kind (`state`, `sample`, `event`,
    /// `stream`, `setpoint`, `read`, `commands`).
    pub kind: String,
    /// Request message identity from the generated port signature.
    pub request_fqn: String,
    /// Observation payload message identity from the generated port signature.
    pub response_fqn: String,
    /// Maximum encoded provider payload admitted by the runtime.
    pub max_message_bytes: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_program_round_trips() {
        let program = BundleScenarioProgram {
            scenario_name: "scenarios/ForwardTurnStop".to_owned(),
            program_path: "program.bin".to_owned(),
            program_byte_length: 256,
            program_digest: "deadbeef".repeat(8),
            fixture_instance_id: "fixture".to_owned(),
            controlled_execution: true,
        };
        let json = serde_json::to_string(&program).expect("serializes");
        let decoded: BundleScenarioProgram = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, program);
    }

    #[test]
    fn scenario_producer_round_trips() {
        let producer = BundleScenarioProducer {
            instance: "fixture".to_owned(),
            port: "pose".to_owned(),
            service_fqn: "phoxal.scenario.v1.Producer".to_owned(),
            method: "Publish".to_owned(),
            kind: "state".to_owned(),
            request_fqn: "google.protobuf.Empty".to_owned(),
            response_fqn: "phoxal.scenario.v1.Pose".to_owned(),
            max_message_bytes: 1024,
        };
        let json = serde_json::to_string(&producer).expect("serializes");
        let decoded: BundleScenarioProducer = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, producer);
    }

    #[test]
    fn scenario_program_rejects_unknown_fields() {
        let json = r#"{
            "scenario_name": "x",
            "program_path": "x",
            "program_byte_length": 1,
            "program_digest": "x",
            "fixture_instance_id": "x",
            "controlled_execution": true,
            "sneaky": true
        }"#;
        let err = serde_json::from_str::<BundleScenarioProgram>(json).expect_err("rejected");
        assert!(err.to_string().contains("unknown field"));
    }
}
