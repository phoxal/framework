//! The `emit-apis` metadata (D50/D57).
//!
//! Every service, driver, tool, and simulator binary exposes a top-level
//! `emit-apis` subcommand that serializes its compiled-in static metadata to
//! stdout as exactly one JSON document and exits - before loading robot config,
//! `.env`, tracing, Zenoh, or running `#[setup]`. `phoxal-cli check`/`deploy
//! build` run it on the resolved artifacts; there is no release descriptor or
//! sidecar file (D57).
//!
//! The emitted JSON schema is **frozen** (fields below). Adding a field is
//! backward-compatible; renaming/removing one is a schema change. There is no
//! `bus_abi` axis to track (D1): the wire ABI dissolved into the
//! generation-qualified key.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::participant::spec::{ContractUse, ParticipantBehavior};

/// The frozen emitted-metadata schema id.
pub const EMIT_SCHEMA: &str = "phoxal.emit-apis/v0";

/// The single JSON document `emit-apis` prints (frozen field set).
#[derive(Debug, Serialize)]
pub struct ParticipantMetadata {
    /// The emitted-schema id.
    pub schema: &'static str,
    /// What this artifact is.
    pub artifact: Artifact,
    /// The framework version that built it.
    pub framework: Framework,
    /// The one API version the artifact runs against.
    pub api_version: String,
    /// Whether normal graph topology applies to this participant.
    pub participant_class: &'static str,
    /// The union of field-side + server-side contracts.
    pub required_contracts: Vec<ContractView>,
    /// The participant's JSON Schema config contract.
    pub config_schema: serde_json::Value,
}

/// Artifact identity.
#[derive(Debug, Serialize)]
pub struct Artifact {
    /// `service` | `driver` | `tool` | `simulator`.
    pub kind: &'static str,
    /// The artifact id (the participant id).
    pub id: String,
}

/// Framework provenance.
#[derive(Debug, Serialize)]
pub struct Framework {
    /// The phoxal framework version.
    pub version: String,
}

/// One contract in the emitted document: its generation-qualified wire key
/// (D1). Request/response bodies of one query topic share a key, so they
/// collapse into one entry here.
#[derive(Debug, Serialize)]
pub struct ContractView {
    /// The generation-qualified wire key (`<Body as ContractBody>::TOPIC`).
    pub topic: String,
}

impl From<&ContractUse> for ContractView {
    fn from(c: &ContractUse) -> Self {
        ContractView {
            topic: c.topic.to_string(),
        }
    }
}

/// Build the metadata document for a participant `R`.
pub fn participant_metadata<R: ParticipantBehavior>() -> ParticipantMetadata {
    let mut seen = BTreeSet::new();
    let required_contracts = R::FIELD_CONTRACTS
        .iter()
        .chain(R::SERVER_CONTRACTS.iter())
        .filter(|contract| seen.insert(contract.topic.to_string()))
        .map(ContractView::from)
        .collect();

    ParticipantMetadata {
        schema: EMIT_SCHEMA,
        artifact: Artifact {
            kind: R::KIND,
            id: R::ID.to_string(),
        },
        framework: Framework {
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        api_version: R::API_VERSION.to_string(),
        participant_class: R::PARTICIPANT_CLASS,
        required_contracts,
        config_schema: serde_json::to_value(schemars::schema_for!(R::Config))
            .expect("config JSON schema is always serializable"),
    }
}

/// Serialize a participant's metadata to a pretty JSON string.
pub fn emit_apis_json<R: ParticipantBehavior>() -> String {
    serde_json::to_string_pretty(&participant_metadata::<R>())
        .expect("ParticipantMetadata is always serializable")
}

/// Print a participant's metadata as one JSON document to stdout.
pub fn print_emit_apis<R: ParticipantBehavior>() {
    println!("{}", emit_apis_json::<R>());
}
