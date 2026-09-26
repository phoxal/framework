//! Strict parsing, normalization, and descriptor resolution for authored
//! service documents (`service.yaml`, schema `phoxal/service/v0`).
//!
//! One service document is the sole authored endpoint authority for its
//! package: local endpoint names, directions, message types, delivery
//! policies, and bounds. Message definitions stay in Protobuf; behavior,
//! configuration, state, and payload construction stay handwritten Rust.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use prost::Message;
use prost_reflect::{DescriptorPool, MessageDescriptor};
use serde::Deserialize;

use crate::Error;

/// Schema identity of an authored service document.
pub const SCHEMA: &str = "phoxal/service/v0";

/// File name of the authored service document beside a package's `api/` tree.
pub const FILE_NAME: &str = "service.yaml";

/// Schema identity of an authored robot document.
pub const ROBOT_SCHEMA: &str = "phoxal/robot/v0";

/// File name of the authored component document beside a package's `api/` tree.
pub const COMPONENT_FILE_NAME: &str = "component.yaml";

/// Schema identity of an authored component document.
pub const COMPONENT_SCHEMA: &str = "phoxal/component/v0";

/// The only built-in message vocabulary mapped to SDK-owned Rust types.
const ROBOTICS_PACKAGE: &str = "phoxal.robotics.v1";
/// The canonical empty message.
const EMPTY_MESSAGE: &str = "google.protobuf.Empty";

/// An authored service document before descriptor resolution.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDocument {
    /// Schema line, present only in a standalone `service.yaml`.
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, InputDecl>,
    #[serde(default)]
    outputs: BTreeMap<String, OutputDecl>,
    #[serde(default)]
    operations: BTreeMap<String, ServedDecl>,
    #[serde(default)]
    calls: BTreeMap<String, CallDecl>,
}

impl ServiceDocument {
    /// Returns whether any referenced message belongs to a package.
    pub fn references_package(&self, package: &str) -> bool {
        let prefix = format!("{package}.");
        let matches = |name: &str| name == package || name.starts_with(&prefix);
        self.inputs.values().any(|input| matches(&input.message))
            || self.outputs.values().any(|output| matches(&output.message))
            || self
                .operations
                .values()
                .any(|endpoint| matches(&endpoint.request) || matches(&endpoint.response))
            || self
                .calls
                .values()
                .any(|endpoint| matches(&endpoint.request) || matches(&endpoint.response))
    }

    /// Returns whether the document declares no endpoints at all.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
            && self.outputs.is_empty()
            && self.operations.is_empty()
            && self.calls.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputDecl {
    #[serde(rename = "type")]
    message: String,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default = "default_true")]
    required: bool,
    max_age_ms: Option<u64>,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
    lease: Option<LeaseDecl>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputDecl {
    #[serde(rename = "type")]
    message: String,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default)]
    retained_latest: bool,
    lease: Option<LeaseDecl>,
    projection: Option<ProjectionMode>,
    #[serde(default)]
    bootstrap: bool,
    #[serde(default)]
    on_change: bool,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServedDecl {
    contract: String,
    request: String,
    response: String,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallDecl {
    contract: String,
    request: String,
    response: String,
    #[serde(default = "default_true")]
    required: bool,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// Delivery policy of one data endpoint.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// One replaceable retained or fresh value.
    #[default]
    Latest,
    /// A bounded ordered batch.
    Queue,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseDecl {
    valid_for_ms: u64,
}

/// Output projection mode, authored as the bare string `projection: state`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum ProjectionMode {
    State,
}

/// A resolved message reference with its Rust path inside the generating crate.
#[derive(Clone, Debug)]
pub struct ResolvedMessage {
    /// Fully-qualified Protobuf message name.
    pub fqn: String,
    /// Rust path of the generated (or SDK-owned) message type.
    pub rust_path: String,
}

impl ResolvedMessage {
    fn resolve(pool: &DescriptorPool, fqn: &str, owner: &str, path: &Path) -> Result<Self, Error> {
        let invalid = |message: String| Error::ApiInput {
            path: path.to_owned(),
            message: format!("{owner} references {fqn}: {message}"),
        };
        if fqn == EMPTY_MESSAGE {
            return Ok(Self {
                fqn: fqn.to_owned(),
                rust_path: "::phoxal::contract::Empty".to_owned(),
            });
        }
        if !qualified_name(fqn) {
            return Err(invalid(
                "the name is not a dotted Protobuf identifier".into(),
            ));
        }
        let descriptor = pool.get_message_by_name(fqn).ok_or_else(|| {
            invalid("no message definition resolves in this service's schemas".into())
        })?;
        let rust_path = if descriptor.parent_file().package_name() == ROBOTICS_PACKAGE {
            format!("::phoxal::robotics::{}", descriptor.name())
        } else {
            rust_message_path(&descriptor)
        };
        Ok(Self {
            fqn: fqn.to_owned(),
            rust_path,
        })
    }
}

fn rust_message_path(message: &MessageDescriptor) -> String {
    let package = message
        .parent_file()
        .package_name()
        .split('.')
        .map(heck::ToSnakeCase::to_snake_case)
        .collect::<Vec<_>>()
        .join("::");
    format!("crate::api::types::{package}::{}", message.name())
}

/// One declared input endpoint after validation and resolution.
#[derive(Clone, Debug)]
pub struct ResolvedInput {
    /// Local endpoint name.
    pub name: String,
    /// Payload message.
    pub message: ResolvedMessage,
    /// Delivery policy.
    pub delivery: Delivery,
    /// Whether a composition must connect this endpoint.
    pub required: bool,
    /// Freshness bound for latest delivery.
    pub max_age_ms: Option<u64>,
    /// Batch bound for queued delivery.
    pub max_items: Option<u64>,
    /// Encoded-byte bound.
    pub max_bytes: u64,
    /// Lease validity for leased inputs.
    pub lease_valid_for_ms: Option<u64>,
}

/// One declared output endpoint after validation and resolution.
#[derive(Clone, Debug)]
pub struct ResolvedOutput {
    /// Local endpoint name.
    pub name: String,
    /// Payload message.
    pub message: ResolvedMessage,
    /// Delivery policy.
    pub delivery: Delivery,
    /// Whether the retained latest value stays addressable.
    pub retained_latest: bool,
    /// Encoded-byte bound.
    pub max_bytes: u64,
    /// Batch bound for queued delivery.
    pub max_items: Option<u64>,
    /// Lease validity for leased outputs.
    pub lease_valid_for_ms: Option<u64>,
    /// Whether publication comes from an implemented state projection hook.
    pub projection: bool,
    /// Whether the projection publishes initialized state before the first step.
    pub bootstrap: bool,
    /// Whether the projection publishes only on payload change.
    pub on_change: bool,
}

/// One declared served operation or required call after resolution.
#[derive(Clone, Debug)]
pub struct ResolvedOperation {
    /// Local endpoint name.
    pub name: String,
    /// Behavioral contract identity, independent of either enclosing service.
    pub contract: String,
    /// Request message.
    pub request: ResolvedMessage,
    /// Response message.
    pub response: ResolvedMessage,
    /// Whether a composition must bind this call.
    pub required: bool,
    /// Outstanding/batch item bound.
    pub max_items: u64,
    /// Encoded-byte bound.
    pub max_bytes: u64,
}

/// A fully resolved service document: the single normalized endpoint
/// authority shared by code generation and composition validation.
#[derive(Clone, Debug)]
pub struct ResolvedService {
    /// Validated input endpoints in authored order.
    pub inputs: Vec<ResolvedInput>,
    /// Validated output endpoints in authored order.
    pub outputs: Vec<ResolvedOutput>,
    /// Validated served operations in authored order.
    pub operations: Vec<ResolvedOperation>,
    /// Validated required calls in authored order.
    pub calls: Vec<ResolvedOperation>,
}

impl ResolvedService {
    /// Returns every endpoint name declared by this service.
    pub fn endpoint_names(&self) -> Vec<&str> {
        self.inputs
            .iter()
            .map(|input| input.name.as_str())
            .chain(self.outputs.iter().map(|output| output.name.as_str()))
            .chain(
                self.operations
                    .iter()
                    .map(|operation| operation.name.as_str()),
            )
            .chain(self.calls.iter().map(|call| call.name.as_str()))
            .collect()
    }
}

/// Parses an authored service document strictly.
///
/// Unknown fields, duplicate mapping keys, invalid names, and invalid
/// combinations are rejected instead of merged or ignored.
pub fn parse_document(source: &[u8], path: &Path) -> Result<ServiceDocument, Error> {
    reject_duplicate_keys(source, path, FILE_NAME)?;
    let document: ServiceDocument =
        serde_yaml::from_slice(source).map_err(|source| Error::ApiInput {
            path: path.to_owned(),
            message: format!("invalid {FILE_NAME}: {source}"),
        })?;
    if document.schema.as_deref() != Some(SCHEMA) {
        return Err(Error::ApiInput {
            path: path.to_owned(),
            message: format!(
                "{FILE_NAME} declares schema {:?}; this build supports {SCHEMA:?}",
                document.schema.unwrap_or_default()
            ),
        });
    }
    Ok(document)
}

/// The brain section of a robot document: the four endpoint sections plus the
/// robot document's carrier fields, validated as strictly as a standalone
/// service document so a typo cannot silently drop the brain's declaration.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrainSection {
    /// Binary target name owned by the robot document, not by endpoints.
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "carrier field of the robot document; consumed by cargo-phoxal"
    )]
    pub binary: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, InputDecl>,
    #[serde(default)]
    outputs: BTreeMap<String, OutputDecl>,
    #[serde(default)]
    operations: BTreeMap<String, ServedDecl>,
    #[serde(default)]
    calls: BTreeMap<String, CallDecl>,
}

impl BrainSection {
    /// Returns the endpoint declaration carried by this section.
    pub fn document(&self) -> ServiceDocument {
        ServiceDocument {
            schema: None,
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            operations: self.operations.clone(),
            calls: self.calls.clone(),
        }
    }
}

/// Extracts the endpoint sections of an authored component document.
///
/// The component document's carrier fields (`model`, `capabilities`,
/// `assets`) belong to the SDK's component DTO, but their presence and the
/// document's exact key set are still validated here so ordinary Cargo
/// generation and `cargo phoxal` accept and reject the same documents.
pub fn parse_component_document(source: &[u8], path: &Path) -> Result<ServiceDocument, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ComponentWire {
        schema: String,
        #[allow(
            dead_code,
            reason = "component DTO carrier; validated by the SDK component document"
        )]
        model: serde_yaml::Value,
        #[allow(
            dead_code,
            reason = "component DTO carrier; validated by the SDK component document"
        )]
        capabilities: BTreeMap<String, serde_yaml::Value>,
        #[serde(default)]
        #[allow(
            dead_code,
            reason = "component DTO carrier; validated by the SDK component document"
        )]
        assets: Vec<PathBuf>,
        #[serde(default)]
        inputs: BTreeMap<String, InputDecl>,
        #[serde(default)]
        outputs: BTreeMap<String, OutputDecl>,
        #[serde(default)]
        operations: BTreeMap<String, ServedDecl>,
        #[serde(default)]
        calls: BTreeMap<String, CallDecl>,
    }

    reject_duplicate_keys(source, path, COMPONENT_FILE_NAME)?;
    let wire: ComponentWire = serde_yaml::from_slice(source).map_err(|source| Error::ApiInput {
        path: path.to_owned(),
        message: format!("invalid {COMPONENT_FILE_NAME}: {source}"),
    })?;
    if wire.schema != COMPONENT_SCHEMA {
        return Err(Error::ApiInput {
            path: path.to_owned(),
            message: format!(
                "{COMPONENT_FILE_NAME} declares schema {:?}; this build supports {COMPONENT_SCHEMA:?}",
                wire.schema
            ),
        });
    }
    Ok(ServiceDocument {
        schema: None,
        inputs: wire.inputs,
        outputs: wire.outputs,
        operations: wire.operations,
        calls: wire.calls,
    })
}

/// Resolves and validates a parsed document against a compiled descriptor pool.
pub fn resolve_document(
    document: &ServiceDocument,
    pool: &DescriptorPool,
    path: &Path,
) -> Result<ResolvedService, Error> {
    let reject = |message: String| Error::ApiInput {
        path: path.to_owned(),
        message,
    };

    let mut names = BTreeSet::new();
    let mut ensure_name = |name: &str, section: &str| -> Result<(), Error> {
        if !snake_identifier(name) {
            return Err(reject(format!(
                "{section} endpoint {name:?} must be a lower-snake identifier"
            )));
        }
        if matches!(
            name,
            "inputs" | "outputs" | "operations" | "methods" | "projections"
        ) {
            return Err(reject(format!(
                "{section} endpoint {name:?} uses a name reserved by generated provider glue"
            )));
        }
        if !names.insert(name.to_owned()) {
            return Err(reject(format!(
                "endpoint {name:?} is declared more than once"
            )));
        }
        Ok(())
    };

    let mut inputs = Vec::new();
    for (name, decl) in &document.inputs {
        ensure_name(name, "inputs")?;
        let owner = format!("input {name:?}");
        if !decl.required {
            return Err(reject(format!(
                "{owner}: optional inputs are not supported in {SCHEMA}; \
                 every input requires a composition binding"
            )));
        }
        let message = ResolvedMessage::resolve(pool, &decl.message, &owner, path)?;
        let max_bytes = require_bound(decl.max_bytes, &owner, "max_bytes", path)?;
        match decl.delivery {
            Delivery::Latest => {
                if decl.max_items.is_some() {
                    return Err(reject(format!("{owner}: latest delivery has no max_items")));
                }
            }
            Delivery::Queue => {
                if decl.lease.is_some() {
                    return Err(reject(format!(
                        "{owner}: queued delivery cannot carry a lease"
                    )));
                }
                if decl.max_age_ms.is_some() {
                    return Err(reject(format!(
                        "{owner}: queued delivery has no max_age_ms"
                    )));
                }
                require_bound(decl.max_items, &owner, "max_items", path)?;
            }
        }
        let lease_valid_for_ms = lease_bound(&decl.lease, &owner, path)?;
        if lease_valid_for_ms.is_some() && decl.max_age_ms.is_some() {
            return Err(reject(format!(
                "{owner}: a leased input is governed by its validity interval; \
                 max_age_ms freshness bounds apply to unleased latest inputs"
            )));
        }
        inputs.push(ResolvedInput {
            name: name.clone(),
            message,
            delivery: decl.delivery,
            required: decl.required,
            max_age_ms: decl.max_age_ms,
            max_items: decl.max_items,
            max_bytes,
            lease_valid_for_ms,
        });
    }

    let mut outputs = Vec::new();
    for (name, decl) in &document.outputs {
        ensure_name(name, "outputs")?;
        let owner = format!("output {name:?}");
        let message = ResolvedMessage::resolve(pool, &decl.message, &owner, path)?;
        let max_bytes = require_bound(decl.max_bytes, &owner, "max_bytes", path)?;
        let lease_valid_for_ms = lease_bound(&decl.lease, &owner, path)?;
        let projection = decl
            .projection
            .is_some_and(|mode| mode == ProjectionMode::State);
        if !projection && decl.lease.is_some() {
            return Err(reject(format!(
                "{owner}: a lease requires `projection: state` (step publication does not renew)"
            )));
        }
        if !projection && (decl.bootstrap || decl.on_change) {
            return Err(reject(format!(
                "{owner}: bootstrap and on_change require `projection: state`"
            )));
        }
        if decl.projection.is_some() && decl.delivery == Delivery::Queue {
            return Err(reject(format!(
                "{owner}: a state projection publishes latest values, not a queue"
            )));
        }
        let max_items = match decl.delivery {
            Delivery::Latest => {
                if decl.max_items.is_some() {
                    return Err(reject(format!("{owner}: latest delivery has no max_items")));
                }
                None
            }
            Delivery::Queue => Some(require_bound(decl.max_items, &owner, "max_items", path)?),
        };
        if decl.retained_latest && decl.delivery != Delivery::Latest {
            return Err(reject(format!(
                "{owner}: retained_latest applies to latest delivery"
            )));
        }
        if projection && decl.retained_latest {
            return Err(reject(format!(
                "{owner}: a state projection is retained by definition; remove retained_latest"
            )));
        }
        if decl.delivery == Delivery::Latest && !projection && !decl.retained_latest {
            return Err(reject(format!(
                "{owner}: latest step publication is retained by definition; \
                 a non-retained stream is `delivery: queue`"
            )));
        }
        outputs.push(ResolvedOutput {
            name: name.clone(),
            message,
            delivery: decl.delivery,
            retained_latest: decl.retained_latest || projection,
            max_bytes,
            max_items,
            lease_valid_for_ms,
            projection,
            bootstrap: decl.bootstrap,
            on_change: decl.on_change,
        });
    }

    let mut operations = Vec::new();
    for (name, decl) in &document.operations {
        ensure_name(name, "operations")?;
        let resolved = resolve_exchange(
            pool,
            name,
            &decl.contract,
            &decl.request,
            &decl.response,
            path,
        )?;
        operations.push(ResolvedOperation {
            name: name.clone(),
            contract: decl.contract.clone(),
            request: resolved.0,
            response: resolved.1,
            required: false,
            max_items: decl.max_items.unwrap_or(1),
            max_bytes: require_bound(
                decl.max_bytes,
                &format!("operation {name:?}"),
                "max_bytes",
                path,
            )?,
        });
    }

    let mut calls = Vec::new();
    for (name, decl) in &document.calls {
        ensure_name(name, "calls")?;
        if !decl.required {
            return Err(reject(format!(
                "call {name:?}: optional requirements are not supported in {SCHEMA}; \
                 every call requires a composition binding"
            )));
        }
        let resolved = resolve_exchange(
            pool,
            name,
            &decl.contract,
            &decl.request,
            &decl.response,
            path,
        )?;
        calls.push(ResolvedOperation {
            name: name.clone(),
            contract: decl.contract.clone(),
            request: resolved.0,
            response: resolved.1,
            required: decl.required,
            max_items: decl.max_items.unwrap_or(1),
            max_bytes: require_bound(decl.max_bytes, &format!("call {name:?}"), "max_bytes", path)?,
        });
    }

    Ok(ResolvedService {
        inputs,
        outputs,
        operations,
        calls,
    })
}

fn resolve_exchange(
    pool: &DescriptorPool,
    name: &str,
    contract: &str,
    request: &str,
    response: &str,
    path: &Path,
) -> Result<(ResolvedMessage, ResolvedMessage), Error> {
    if !qualified_name(contract) || !contract.contains('.') {
        return Err(Error::ApiInput {
            path: path.to_owned(),
            message: format!(
                "endpoint {name:?} contract {contract:?} must be a dotted fully-qualified name"
            ),
        });
    }
    let owner = format!("endpoint {name:?}");
    let request = ResolvedMessage::resolve(pool, request, &owner, path)?;
    let response = ResolvedMessage::resolve(pool, response, &owner, path)?;
    Ok((request, response))
}

fn require_bound(value: Option<u64>, owner: &str, field: &str, path: &Path) -> Result<u64, Error> {
    let bound = value.ok_or_else(|| Error::ApiInput {
        path: path.to_owned(),
        message: format!("{owner} requires a positive {field}"),
    })?;
    if bound == 0 {
        return Err(Error::ApiInput {
            path: path.to_owned(),
            message: format!("{owner} requires {field} > 0"),
        });
    }
    Ok(bound)
}

fn lease_bound(lease: &Option<LeaseDecl>, owner: &str, path: &Path) -> Result<Option<u64>, Error> {
    lease
        .as_ref()
        .map(|lease| {
            if lease.valid_for_ms == 0 {
                return Err(Error::ApiInput {
                    path: path.to_owned(),
                    message: format!("{owner} lease valid_for_ms must be positive"),
                });
            }
            Ok(lease.valid_for_ms)
        })
        .transpose()
}

/// Rejects duplicate mapping keys before typed decoding so a repeated
/// endpoint cannot silently replace its earlier declaration.
///
/// Key lines (`name:` with no inline value) open nested mappings; the
/// indent-stack reconstruction is only a pre-check for duplicates, while
/// typed decoding remains the authority for every other document rule.
pub(crate) fn reject_duplicate_keys(source: &[u8], path: &Path, owner: &str) -> Result<(), Error> {
    // The first element is a permanent root sentinel so top-level keys are
    // also checked; its indent never matches a real line.
    let mut stack: Vec<(usize, BTreeSet<String>)> = vec![(usize::MAX, BTreeSet::new())];
    for line in String::from_utf8_lossy(source).lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let Some(key) = split_key(trimmed) else {
            continue;
        };
        while stack.len() > 1 && stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }
        if let Some((_, keys)) = stack.last_mut()
            && !keys.insert(key.to_owned())
        {
            return Err(Error::ApiInput {
                path: path.to_owned(),
                message: format!("{owner} declares key {key:?} more than once in one mapping"),
            });
        }
        stack.push((indent, BTreeSet::new()));
        if stack.len() > 64 {
            return Err(Error::ApiInput {
                path: path.to_owned(),
                message: format!("{owner} nests deeper than 64 levels"),
            });
        }
    }
    Ok(())
}

/// Returns the mapping key of a line that opens a nested block, if any.
fn split_key(trimmed: &str) -> Option<&str> {
    if trimmed.starts_with('-') {
        return None;
    }
    let (key, rest) = trimmed.split_once(':')?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(key.trim().trim_matches('"').trim_matches('\''))
}

fn qualified_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|segment| {
            let mut characters = segment.chars();
            characters
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn snake_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A resolved declaration plus the descriptor closure it compiled against.
#[derive(Clone, Debug)]
pub struct DeclarationEvidence {
    /// The normalized endpoint authority.
    pub service: ResolvedService,
    /// Raw descriptor-set bytes for definition equality checks.
    pub descriptors: Vec<u8>,
}

/// Kind of one declared endpoint, for composition checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointSide {
    /// The endpoint receives data.
    Input,
    /// The endpoint produces data.
    Output,
    /// The endpoint serves an operation.
    Served,
    /// The endpoint requires an operation.
    Call,
}

impl ResolvedService {
    /// Locates one declared endpoint by local name.
    pub fn endpoint(&self, name: &str) -> Option<(EndpointSide, &str)> {
        let message = self
            .inputs
            .iter()
            .find(|input| input.name == name)
            .map(|input| input.message.fqn.as_str())
            .map(|fqn| (EndpointSide::Input, fqn));
        let output = self
            .outputs
            .iter()
            .find(|output| output.name == name)
            .map(|output| output.message.fqn.as_str())
            .map(|fqn| (EndpointSide::Output, fqn));
        let served = self
            .operations
            .iter()
            .find(|operation| operation.name == name)
            .map(|operation| operation.response.fqn.as_str())
            .map(|fqn| (EndpointSide::Served, fqn));
        let call = self
            .calls
            .iter()
            .find(|call| call.name == name)
            .map(|call| call.response.fqn.as_str())
            .map(|fqn| (EndpointSide::Call, fqn));
        message.or(output).or(served).or(call)
    }

    /// Returns the input declaration for one local endpoint name.
    pub fn input(&self, name: &str) -> Option<&ResolvedInput> {
        self.inputs.iter().find(|input| input.name == name)
    }

    /// Returns the required-operation declaration for one local endpoint name.
    pub fn call(&self, name: &str) -> Option<&ResolvedOperation> {
        self.calls.iter().find(|call| call.name == name)
    }

    /// Returns the data output declaration for one local endpoint name.
    pub fn output(&self, name: &str) -> Option<&ResolvedOutput> {
        self.outputs.iter().find(|output| output.name == name)
    }

    /// Returns the served operation declaration for one local endpoint name.
    pub fn operation(&self, name: &str) -> Option<&ResolvedOperation> {
        self.operations
            .iter()
            .find(|operation| operation.name == name)
    }
}

/// Reports whether a message resolves to identical definitions in two
/// declarations' descriptor closures.
fn definitions_agree(
    consumer: &DeclarationEvidence,
    producer: &DeclarationEvidence,
    fqn: &str,
) -> Result<bool, Error> {
    if fqn == EMPTY_MESSAGE {
        return Ok(true);
    }
    let left = DescriptorPool::decode(consumer.descriptors.as_slice())
        .ok()
        .and_then(|pool| pool.get_message_by_name(fqn))
        .map(|message| message.descriptor_proto().encode_to_vec());
    let right = DescriptorPool::decode(producer.descriptors.as_slice())
        .ok()
        .and_then(|pool| pool.get_message_by_name(fqn))
        .map(|message| message.descriptor_proto().encode_to_vec());
    Ok(matches!((left, right), (Some(left), Some(right)) if left == right))
}

/// Validates one data binding between a consumer input and a producer output.
///
/// The check compares message contract identity and definitions, delivery
/// compatibility, lease requirements, and declared bounds without depending
/// on either side's enclosing service identity.
pub fn check_data_binding(
    consumer: &DeclarationEvidence,
    consumer_endpoint: &str,
    producer: &DeclarationEvidence,
    producer_endpoint: &str,
) -> Result<(), Error> {
    let describe = |message: String| Error::ApiInput {
        path: Path::new("robot.yaml").to_owned(),
        message,
    };
    let input = consumer.service.input(consumer_endpoint).ok_or_else(|| {
        describe(format!(
            "consumer endpoint `{consumer_endpoint}` is not a declared input"
        ))
    })?;
    let output = producer.service.output(producer_endpoint).ok_or_else(|| {
        describe(format!(
            "producer endpoint `{producer_endpoint}` is not a declared data output"
        ))
    })?;
    if input.message.fqn != output.message.fqn {
        return Err(describe(format!(
            "input `{consumer_endpoint}` expects `{}` but `{producer_endpoint}` produces `{}`",
            input.message.fqn, output.message.fqn
        )));
    }
    if !definitions_agree(consumer, producer, &input.message.fqn)? {
        return Err(describe(format!(
            "message `{}` differs between the two declarations",
            input.message.fqn
        )));
    }
    if input.delivery != output.delivery {
        return Err(describe(format!(
            "`{consumer_endpoint}` uses {:?} delivery but `{producer_endpoint}` uses {:?}",
            input.delivery, output.delivery
        )));
    }
    match (input.lease_valid_for_ms, output.lease_valid_for_ms) {
        (None, None) => {}
        (Some(left), Some(right)) if left == right => {}
        (left, right) => {
            return Err(describe(format!(
                "lease requirements differ between `{consumer_endpoint}` ({left:?}) and \
                 `{producer_endpoint}` ({right:?})"
            )));
        }
    }
    if input.max_bytes < output.max_bytes {
        return Err(describe(format!(
            "`{consumer_endpoint}` accepts {} bytes but `{producer_endpoint}` may publish {}",
            input.max_bytes, output.max_bytes
        )));
    }
    Ok(())
}

/// Validates one operation binding between a required call and a served
/// operation.
///
/// Identity is the declared contract plus request and response messages;
/// equality of the whole enclosing services is deliberately not required.
pub fn check_call_binding(
    consumer: &DeclarationEvidence,
    consumer_endpoint: &str,
    producer: &DeclarationEvidence,
    producer_endpoint: &str,
) -> Result<(), Error> {
    let describe = |message: String| Error::ApiInput {
        path: Path::new("robot.yaml").to_owned(),
        message,
    };
    let call = consumer.service.call(consumer_endpoint).ok_or_else(|| {
        describe(format!(
            "consumer endpoint `{consumer_endpoint}` is not a declared call"
        ))
    })?;
    let operation = producer
        .service
        .operation(producer_endpoint)
        .ok_or_else(|| {
            describe(format!(
                "producer endpoint `{producer_endpoint}` is not a declared operation"
            ))
        })?;
    if call.contract != operation.contract {
        return Err(describe(format!(
            "call `{consumer_endpoint}` requires contract `{}` but `{producer_endpoint}` serves `{}`",
            call.contract, operation.contract
        )));
    }
    if call.request.fqn != operation.request.fqn || call.response.fqn != operation.response.fqn {
        return Err(describe(format!(
            "call `{consumer_endpoint}` exchanges {}/{} but `{producer_endpoint}` serves {}/{}",
            call.request.fqn, call.response.fqn, operation.request.fqn, operation.response.fqn
        )));
    }
    if !definitions_agree(consumer, producer, &call.response.fqn)? {
        return Err(describe(format!(
            "message `{}` differs between the two declarations",
            call.response.fqn
        )));
    }
    if call.max_bytes < operation.max_bytes {
        return Err(describe(format!(
            "`{consumer_endpoint}` accepts {} reply bytes but `{producer_endpoint}` may send {}",
            call.max_bytes, operation.max_bytes
        )));
    }
    Ok(())
}

/// Validates one explicit observation projection between a foreign producer
/// output and a consumer latest input.
///
/// Only top-level scalar copies between compatible explicit-presence fields
/// are supported; every destination field must be accounted for and absent
/// optional values stay absent.  Anything else is rejected with a diagnostic
/// instead of a silent fallback.
pub fn check_projection_binding(
    consumer: &DeclarationEvidence,
    consumer_endpoint: &str,
    producer: &DeclarationEvidence,
    producer_endpoint: &str,
    map: &std::collections::BTreeMap<String, String>,
) -> Result<(), Error> {
    let describe = |message: String| Error::ApiInput {
        path: Path::new("robot.yaml").to_owned(),
        message,
    };
    if map.is_empty() {
        return Err(describe(
            "a projection connection requires at least one field mapping".into(),
        ));
    }
    let input = consumer.service.input(consumer_endpoint).ok_or_else(|| {
        describe(format!(
            "consumer endpoint `{consumer_endpoint}` is not a declared input"
        ))
    })?;
    if input.delivery != Delivery::Latest || input.lease_valid_for_ms.is_some() {
        return Err(describe(format!(
            "`{consumer_endpoint}` must be a plain latest input for an observation projection"
        )));
    }
    let output = producer.service.output(producer_endpoint).ok_or_else(|| {
        describe(format!(
            "producer endpoint `{producer_endpoint}` is not a declared data output"
        ))
    })?;
    let consumer_pool = DescriptorPool::decode(consumer.descriptors.as_slice())
        .map_err(|error| describe(format!("consumer descriptors are invalid: {error}")))?;
    let producer_pool = DescriptorPool::decode(producer.descriptors.as_slice())
        .map_err(|error| describe(format!("producer descriptors are invalid: {error}")))?;
    let destination = consumer_pool
        .get_message_by_name(&input.message.fqn)
        .ok_or_else(|| {
            describe(format!(
                "consumer message `{}` is missing",
                input.message.fqn
            ))
        })?;
    let source = producer_pool
        .get_message_by_name(&output.message.fqn)
        .ok_or_else(|| {
            describe(format!(
                "producer message `{}` is missing",
                output.message.fqn
            ))
        })?;

    let unsupported = |side: &str, field: &str| {
        describe(format!(
            "projection field `{field}` on the {side} message must be a top-level scalar"
        ))
    };
    let mut mapped_destination: Vec<String> = Vec::new();
    for (destination_path, source_path) in map {
        if destination_path.contains('.') || source_path.contains('.') {
            return Err(describe(
                "nested field paths are not supported in projection mappings".into(),
            ));
        }
        let destination_field = destination
            .get_field_by_name(destination_path)
            .ok_or_else(|| describe(format!("destination has no field `{destination_path}`")))?;
        let source_field = source
            .get_field_by_name(source_path)
            .ok_or_else(|| describe(format!("source has no field `{source_path}`")))?;
        let repeated = |field: &prost_reflect::FieldDescriptor| {
            field.field_descriptor_proto().label()
                == prost_types::field_descriptor_proto::Label::Repeated
        };
        if repeated(&destination_field) || repeated(&source_field) {
            return Err(describe(
                "repeated and mapped fields are not supported in projection mappings".into(),
            ));
        }
        let is_composite = |field: &prost_reflect::FieldDescriptor| {
            matches!(
                field.field_descriptor_proto().r#type(),
                prost_types::field_descriptor_proto::Type::Enum
                    | prost_types::field_descriptor_proto::Type::Message
                    | prost_types::field_descriptor_proto::Type::Group
            )
        };
        if is_composite(&destination_field) || is_composite(&source_field) {
            return Err(describe(
                "enum, message, and oneof fields are not supported in projection mappings".into(),
            ));
        }
        if !matches!(
            destination_field.field_descriptor_proto().r#type(),
            prost_types::field_descriptor_proto::Type::Double
                | prost_types::field_descriptor_proto::Type::Float
                | prost_types::field_descriptor_proto::Type::Int32
                | prost_types::field_descriptor_proto::Type::Int64
                | prost_types::field_descriptor_proto::Type::Uint32
                | prost_types::field_descriptor_proto::Type::Uint64
                | prost_types::field_descriptor_proto::Type::Bool
                | prost_types::field_descriptor_proto::Type::String
                | prost_types::field_descriptor_proto::Type::Bytes
        ) || !matches!(
            source_field.field_descriptor_proto().r#type(),
            prost_types::field_descriptor_proto::Type::Double
                | prost_types::field_descriptor_proto::Type::Float
                | prost_types::field_descriptor_proto::Type::Int32
                | prost_types::field_descriptor_proto::Type::Int64
                | prost_types::field_descriptor_proto::Type::Uint32
                | prost_types::field_descriptor_proto::Type::Uint64
                | prost_types::field_descriptor_proto::Type::Bool
                | prost_types::field_descriptor_proto::Type::String
                | prost_types::field_descriptor_proto::Type::Bytes
        ) {
            return Err(unsupported("mapped", destination_path));
        }
        if destination_field.field_descriptor_proto().r#type()
            != source_field.field_descriptor_proto().r#type()
        {
            return Err(describe(format!(
                "projection field `{destination_path}` copies a different scalar type than `{source_path}`"
            )));
        }
        let destination_optional = destination_field.field_descriptor_proto().proto3_optional();
        let source_optional = source_field.field_descriptor_proto().proto3_optional();
        if !destination_optional && source_optional {
            return Err(describe(format!(
                "destination field `{destination_path}` is required but `{source_path}` is optional; absent values cannot be fabricated"
            )));
        }
        mapped_destination.push(destination_path.clone());
    }
    let mut unmapped = Vec::new();
    for field in destination.fields() {
        let optional = field.field_descriptor_proto().proto3_optional();
        let mapped = mapped_destination.contains(&field.name().to_owned());
        if !mapped && !optional {
            unmapped.push(field.name().to_owned());
        }
    }
    if !unmapped.is_empty() {
        unmapped.sort();
        return Err(describe(format!(
            "required destination fields are missing from the mapping: {}",
            unmapped.join(", ")
        )));
    }
    Ok(())
}
