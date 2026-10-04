//! Runtime artifact record family.
//!
//! Owns the inert contract records that describe a user's compiled
//! artifact's runtime shape, inputs, and outputs. These records appear
//! inside the `BundleArtifact` and are also exchanged by the
//! connection-validator and the supervisor when admitting a connection.
//!
//! Native binary inspection (`ArtifactContract`, `DescriptorInfo`,
//! `inspect_file`, `inspect_bytes`) and connection validation
//! (`validate_connected_endpoints`) remain in `cargo-phoxal`'s tool
//! layer because they read native ELF/Mach-O sections, decode the
//! descriptor pool, and consult the authored `RobotDocument`.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Runtime-record discriminator written by `phoxal-macros`.
pub const RUNTIME_RECORD: &str = "runtime";

/// Manifest-safe artifact contract information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSummary {
    /// Runtime metadata and checked bindings.
    pub runtime: RuntimeRecord,
    /// Digest and descriptor-file inventory for each retained closure.
    pub descriptors: Vec<DescriptorSummary>,
}

/// Manifest-safe descriptor inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorSummary {
    /// SHA-256 digest of the original encoded descriptor bytes.
    pub sha256: String,
    /// Exact descriptor-set byte count.
    pub bytes: u64,
    /// File names retained in the descriptor closure.
    pub files: Vec<String>,
}

/// Runtime timing, config-schema, and checked binding facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum RuntimeRecord {
    /// The first runtime-artifact generation.
    #[serde(rename = "phoxal/artifact/v0")]
    V0 {
        /// Runtime record discriminator.
        record: String,
        /// Logical runtime period in milliseconds.
        period_ms: u64,
        /// Complete invocation deadline in milliseconds.
        timeout_ms: u64,
        /// Initialization deadline in milliseconds.
        init_timeout_ms: u64,
        /// The exact JSON Schema admitted by the runtime configuration type.
        config_schema: serde_json::Value,
        /// Runtime input bindings in source order.
        inputs: Vec<InputRecord>,
        /// Compiled robot conversion routes, using the actual generated endpoints.
        #[serde(default)]
        conversions: Vec<ConversionRoute>,
        /// Public provided endpoint contracts and publication bounds.
        outputs: Vec<OutputRecord>,
    },
}

/// A compiled conversion executed by the robot runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversionRoute {
    /// Authored source endpoint, including its instance.
    pub producer: String,
    /// Authored consuming endpoint, including its instance.
    pub consumer: String,
    /// Actual input endpoint generated on the brain.
    pub input_endpoint: String,
    /// Actual output endpoint generated on the brain.
    pub output_endpoint: String,
}

/// One checked runtime input binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRecord {
    /// Public consumer endpoint key.
    pub name: String,
    /// Required endpoint delivery semantics.
    pub delivery: InputDelivery,
    /// Optional latest-value age bound.
    pub max_age_ms: Option<u64>,
    /// Optional item-count bound.
    pub max_items: Option<u64>,
    /// Optional encoded-byte bound.
    pub max_bytes: Option<u64>,
    /// Public port name for an explicitly bound input.
    pub port: Option<String>,
    /// Complete generated port identity when one is bound.
    pub signature: Option<MethodSignature>,
    /// Expected generated Protobuf request identity, when this input sends requests.
    pub request_fqn: Option<String>,
    /// Expected generated Protobuf publication or response identity.
    pub response_fqn: Option<String>,
    /// Resolved response byte bound for a provided call ingress.
    pub response_max_bytes: Option<u64>,
    /// Resolved response item bound for a provided call ingress.
    pub response_max_items: Option<u64>,
}

/// One checked runtime output binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRecord {
    /// Public provided endpoint key.
    pub name: String,
    /// Public served port name.
    pub port: Option<String>,
    /// Complete generated port identity when one is served.
    pub signature: Option<MethodSignature>,
    /// Item-count bound.
    pub max_items: Option<u64>,
    /// Encoded-byte bound.
    pub max_bytes: Option<u64>,
    /// Encoded request bound.
    pub max_request_bytes: Option<u64>,
    /// Periodic projection cadence.
    pub every_steps: Option<u64>,
    /// Bootstrap publication marker.
    pub bootstrap: bool,
    /// Handler deadline.
    pub timeout_ms: Option<u64>,
}

#[cfg(test)]
use crate::contracts::MethodShape;

/// Delivery requirements of a public consuming endpoint.
///
/// Method identity and call/observation shape live in [`MethodSignature`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum InputDelivery {
    /// One coalesced observation value.
    #[serde(rename = "observation_latest")]
    ObservationLatest,
    /// Ordered observation history.
    #[serde(rename = "observation_history")]
    ObservationHistory,
    /// One replaceable value governed by a finite lease.
    #[serde(rename = "leased_value")]
    LeasedValue,
    /// Ingress queue for generated service calls.
    #[serde(rename = "call_ingress")]
    CallIngress,
    /// Completion of a projection-style call adapter.
    #[serde(rename = "call_result")]
    CallResult,
    /// Binding from an outgoing call to its admitted receiver.
    #[serde(rename = "call_target")]
    CallTarget,
    /// Generated service-call completions.
    #[serde(rename = "call_completions")]
    CallCompletions,
}

pub use crate::contracts::OwnedMethodSignature as MethodSignature;

#[cfg(test)]
mod tests {
    //! Round-trip and discriminator tests for the artifact record family.
    //!
    //! These guarantee that the canonical wire format of `ArtifactSummary`
    //! survives any future internal refactor of the artifact module.

    use super::*;

    fn sample_runtime() -> RuntimeRecord {
        RuntimeRecord::V0 {
            record: RUNTIME_RECORD.to_owned(),
            conversions: Vec::new(),
            period_ms: 20,
            timeout_ms: 100,
            init_timeout_ms: 1_000,
            config_schema: serde_json::json!({"type": "object"}),
            inputs: vec![InputRecord {
                name: "input".to_owned(),
                delivery: InputDelivery::ObservationLatest,
                max_age_ms: None,
                max_items: None,
                max_bytes: None,
                port: None,
                signature: None,
                request_fqn: None,
                response_fqn: None,
                response_max_bytes: None,
                response_max_items: None,
            }],
            outputs: vec![OutputRecord {
                name: "output".to_owned(),
                port: Some("output".to_owned()),
                signature: Some(MethodSignature {
                    endpoint: "output".to_owned(),
                    service: "example.Service".to_owned(),
                    method: "Output".to_owned(),
                    shape: MethodShape::Observation,
                    request: "google.protobuf.Empty".to_owned(),
                    response: "example.Payload".to_owned(),
                    retained_latest: true,
                    lease_valid_for_ms: None,
                }),
                max_items: None,
                max_bytes: Some(1024),
                max_request_bytes: None,
                every_steps: None,
                bootstrap: false,
                timeout_ms: None,
            }],
        }
    }

    #[test]
    fn runtime_record_round_trips_with_deny_unknown_fields() {
        let value = sample_runtime();
        let json = serde_json::to_string(&value).expect("serializes");
        let decoded: RuntimeRecord = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, value);
    }

    #[test]
    fn runtime_record_rejects_unknown_fields() {
        let value = sample_runtime();
        let mut json: serde_json::Value = serde_json::to_value(&value).expect("value");
        json.as_object_mut()
            .expect("object")
            .insert("sneaky".to_owned(), serde_json::json!(42));
        let text = serde_json::to_string(&json).expect("text");
        let err = serde_json::from_str::<RuntimeRecord>(&text).expect_err("rejected");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn method_shape_serializes_as_lowercase() {
        for (kind, expected) in [
            (MethodShape::Call, "\"call\""),
            (MethodShape::Observation, "\"observation\""),
        ] {
            let json = serde_json::to_string(&kind).expect("serializes");
            assert_eq!(
                json, expected,
                "MethodShape {:?} serializes as {expected}",
                kind
            );
        }
    }

    #[test]
    fn artifact_summary_round_trips() {
        let summary = ArtifactSummary {
            runtime: sample_runtime(),
            descriptors: vec![DescriptorSummary {
                sha256: "deadbeef".repeat(8),
                bytes: 1_234,
                files: vec!["phoxal/bootstrap/v1/bootstrap.proto".to_owned()],
            }],
        };
        let json = serde_json::to_string(&summary).expect("serializes");
        let decoded: ArtifactSummary = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, summary);
    }
}
