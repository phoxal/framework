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

/// Schema discriminator recorded by the runtime artifact section.
pub const ARTIFACT_SCHEMA: &str = "phoxal/artifact/v0";

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
#[serde(deny_unknown_fields)]
pub struct RuntimeRecord {
    /// Artifact schema discriminator.
    pub schema: String,
    /// Runtime record discriminator.
    pub record: String,
    /// Logical runtime period in milliseconds.
    pub period_ms: u64,
    /// Complete invocation deadline in milliseconds.
    pub timeout_ms: u64,
    /// Initialization deadline in milliseconds.
    pub init_timeout_ms: u64,
    /// The exact JSON Schema admitted by the runtime configuration type.
    pub config_schema: serde_json::Value,
    /// Runtime input bindings in source order.
    pub inputs: Vec<InputRecord>,
    /// Transient per-invocation outputs in source order.
    pub transient_outputs: Vec<OutputRecord>,
    /// Service projection and handler bindings in source order.
    pub service_outputs: Vec<OutputRecord>,
}

/// One checked runtime input binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRecord {
    /// Private Rust input field name.
    pub name: String,
    /// Input semantic form.
    pub kind: InputKind,
    /// Optional latest-value age bound.
    pub max_age_ms: Option<u64>,
    /// Optional item-count bound.
    pub max_items: Option<u64>,
    /// Optional encoded-byte bound.
    pub max_bytes: Option<u64>,
    /// Public port name for an explicitly bound input.
    pub port: Option<String>,
    /// Complete generated port identity when one is bound.
    pub signature: Option<PortSignature>,
    /// Expected generated Protobuf request identity, when this input sends requests.
    pub request_fqn: Option<String>,
    /// Expected generated Protobuf publication or response identity.
    pub response_fqn: Option<String>,
}

/// One checked runtime output binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRecord {
    /// Private Rust field or method name.
    pub name: String,
    /// Output role.
    pub kind: OutputKind,
    /// Public served port name.
    pub port: Option<String>,
    /// Complete generated port identity when one is served.
    pub signature: Option<PortSignature>,
    /// Input field selected by a reply, activation, or worker.
    pub input: Option<String>,
    /// Projection method selected by an offered read.
    pub project: Option<String>,
    /// Item-count bound.
    pub max_items: Option<u64>,
    /// Encoded-byte bound.
    pub max_bytes: Option<u64>,
    /// Encoded request bound.
    pub max_request_bytes: Option<u64>,
    /// Periodic projection cadence.
    pub every_steps: Option<u64>,
    /// Change-gated publication marker.
    pub on_change: bool,
    /// Bootstrap publication marker.
    pub bootstrap: bool,
    /// Setpoint validity duration.
    pub valid_for_ms: Option<u64>,
    /// Handler deadline.
    pub timeout_ms: Option<u64>,
    /// Operation retirement grace.
    pub cancel_grace_ms: Option<u64>,
}

/// The seven public Protobuf port kinds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    /// Latest state projection.
    State,
    /// Captured sample publication.
    Sample,
    /// Discrete event publication.
    Event,
    /// Ordered stream publication.
    Stream,
    /// Replaceable setpoint publication.
    Setpoint,
    /// Unary immutable read.
    Read,
    /// Unary behavioral command.
    Commands,
}

/// Input semantic forms recorded by the runtime macro.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    /// Latest value.
    Latest,
    /// Ordered samples.
    Samples,
    /// Ordered events.
    Events,
    /// Setpoint value.
    Setpoint,
    /// Ordered stream.
    Stream,
    /// Commands.
    Commands,
    /// Immutable read completion.
    Read,
    /// Behavioral request completion.
    Request,
    /// Local operation completion.
    Operation,
}

/// Output roles recorded by the runtime macro.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    /// State projection.
    State,
    /// Sample batch.
    Sample,
    /// Event batch.
    Event,
    /// Stream batch.
    Stream,
    /// Setpoint projection.
    Setpoint,
    /// Read handler.
    Read,
    /// Command reply.
    Reply,
    /// Activation selector.
    Activate,
    /// Operation worker.
    Operation,
}

/// Complete method identity for one generated public port.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSignature {
    /// Public port name.
    pub name: String,
    /// Fully-qualified Protobuf service name.
    pub service: String,
    /// Protobuf method name.
    pub method: String,
    /// Semantic port kind.
    pub kind: PortKind,
    /// Fully-qualified request message name.
    pub request: String,
    /// Fully-qualified response message name.
    pub response: String,
}

#[cfg(test)]
mod tests {
    //! Round-trip and discriminator tests for the artifact record family.
    //!
    //! These guarantee that the canonical wire format of `ArtifactSummary`
    //! survives any future internal refactor of the artifact module.

    use super::*;

    fn sample_runtime() -> RuntimeRecord {
        RuntimeRecord {
            schema: ARTIFACT_SCHEMA.to_owned(),
            record: RUNTIME_RECORD.to_owned(),
            period_ms: 20,
            timeout_ms: 100,
            init_timeout_ms: 1_000,
            config_schema: serde_json::json!({"type": "object"}),
            inputs: vec![InputRecord {
                name: "input".to_owned(),
                kind: InputKind::Latest,
                max_age_ms: None,
                max_items: None,
                max_bytes: None,
                port: None,
                signature: None,
                request_fqn: None,
                response_fqn: None,
            }],
            transient_outputs: Vec::new(),
            service_outputs: vec![OutputRecord {
                name: "output".to_owned(),
                kind: OutputKind::State,
                port: Some("output".to_owned()),
                signature: Some(PortSignature {
                    name: "output".to_owned(),
                    service: "example.Service".to_owned(),
                    method: "Output".to_owned(),
                    kind: PortKind::State,
                    request: "google.protobuf.Empty".to_owned(),
                    response: "example.Payload".to_owned(),
                }),
                input: None,
                project: None,
                max_items: None,
                max_bytes: Some(1024),
                max_request_bytes: None,
                every_steps: None,
                on_change: false,
                bootstrap: false,
                valid_for_ms: None,
                timeout_ms: None,
                cancel_grace_ms: None,
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
        let mut value = sample_runtime();
        value.schema = ARTIFACT_SCHEMA.to_owned();
        let mut json: serde_json::Value = serde_json::to_value(&value).expect("value");
        json.as_object_mut()
            .expect("object")
            .insert("sneaky".to_owned(), serde_json::json!(42));
        let text = serde_json::to_string(&json).expect("text");
        let err = serde_json::from_str::<RuntimeRecord>(&text).expect_err("rejected");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn port_kind_serializes_as_lowercase() {
        for (kind, expected) in [
            (PortKind::State, "\"state\""),
            (PortKind::Sample, "\"sample\""),
            (PortKind::Event, "\"event\""),
            (PortKind::Stream, "\"stream\""),
            (PortKind::Setpoint, "\"setpoint\""),
            (PortKind::Read, "\"read\""),
            (PortKind::Commands, "\"commands\""),
        ] {
            let json = serde_json::to_string(&kind).expect("serializes");
            assert_eq!(
                json, expected,
                "PortKind {:?} serializes as {expected}",
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
