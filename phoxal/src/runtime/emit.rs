//! The `emit-apis` metadata (D50/D57).
//!
//! Every runtime/tool/driver binary exposes a top-level `emit-apis` subcommand
//! that serializes its compiled-in static metadata to stdout as exactly one JSON
//! document and exits — before loading robot config, `.env`, tracing, Zenoh, or
//! running `#[setup]`. `phoxal-cli check`/`deploy build` run it on the resolved
//! artifacts; there is no release descriptor or sidecar file (D57).
//!
//! The emitted JSON schema is **frozen** (fields below). Adding a field is
//! backward-compatible; renaming/removing one is a `bus_abi`/schema change.

use serde::Serialize;

use crate::bus::BUS_ABI;
use crate::runtime::spec::{ContractUse, Direction, RuntimeBehavior};

/// The frozen emitted-metadata schema id.
pub const EMIT_SCHEMA: &str = "phoxal.emit-apis/v0";

/// The single JSON document `emit-apis` prints (frozen field set).
#[derive(Debug, Serialize)]
pub struct RuntimeMetadata {
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
    /// The runtime's JSON Schema config contract.
    pub config_schema: serde_json::Value,
}

/// Artifact identity.
#[derive(Debug, Serialize)]
pub struct Artifact {
    /// `runtime` | `tool` | `driver`.
    pub kind: &'static str,
    /// The artifact id (the runtime id).
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
            family: c.family.to_string(),
            topic: c.topic.to_string(),
            direction: c.direction,
        }
    }
}

/// Build the metadata document for a runtime `R`.
pub fn runtime_metadata<R: RuntimeBehavior>() -> RuntimeMetadata {
    let required_contracts = R::FIELD_CONTRACTS
        .iter()
        .chain(R::SERVER_CONTRACTS.iter())
        .map(ContractView::from)
        .collect();

    RuntimeMetadata {
        schema: EMIT_SCHEMA,
        artifact: Artifact {
            kind: "runtime",
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

/// Serialize a runtime's metadata to a pretty JSON string.
pub fn emit_apis_json<R: RuntimeBehavior>() -> String {
    serde_json::to_string_pretty(&runtime_metadata::<R>())
        .expect("RuntimeMetadata is always serializable")
}

/// Print a runtime's metadata as one JSON document to stdout.
pub fn print_emit_apis<R: RuntimeBehavior>() {
    println!("{}", emit_apis_json::<R>());
}
