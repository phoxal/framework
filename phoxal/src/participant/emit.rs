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
//! backward-compatible; renaming/removing one is a `bus_abi`/schema change.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::bus::BUS_ABI;
use crate::participant::spec::{ContractUse, Direction, ParticipantBehavior};

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
    /// The framework-owned wire-envelope id.
    pub bus_abi: String,
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

/// One contract in the emitted document.
#[derive(Debug, Serialize)]
pub struct ContractView {
    /// API version this contract body belongs to.
    pub api_version: String,
    /// Transitive wire-shape id for this contract body.
    pub schema_id: String,
    /// Contract family id.
    pub family: String,
    /// Versionless topic key.
    pub topic: String,
    /// Direction of use.
    pub direction: Direction,
}

impl From<&ContractUse> for ContractView {
    fn from(c: &ContractUse) -> Self {
        ContractView {
            api_version: c.api_version.to_string(),
            schema_id: c.schema_id.to_string(),
            family: c.family.to_string(),
            topic: c.topic.to_string(),
            direction: c.direction,
        }
    }
}

/// Build the metadata document for a participant `R`.
pub fn participant_metadata<R: ParticipantBehavior>() -> ParticipantMetadata {
    let mut seen = BTreeSet::new();
    let required_contracts = R::FIELD_CONTRACTS
        .iter()
        .chain(R::SERVER_CONTRACTS.iter())
        .filter(|contract| {
            seen.insert((
                contract.api_version.to_string(),
                contract.schema_id.to_string(),
                contract.family.to_string(),
                contract.topic.to_string(),
                contract.direction,
            ))
        })
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
        bus_abi: BUS_ABI.id().to_string(),
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
