//! Strict parsing, normalization, and descriptor resolution for authored
//! capability-derived endpoint records and their private resolver types.
//!
//! Component capabilities derive standard endpoint records. Runnable
//! packages author their additional endpoint contracts in Rust.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Error;

/// Schema identity of an authored robot document.
pub const ROBOT_SCHEMA: &str = "phoxal/robot/v0";

/// File name of a package-owned component document.
pub const COMPONENT_FILE_NAME: &str = "component.yaml";

/// Schema identity of an authored component document.
pub const COMPONENT_SCHEMA: &str = "phoxal/component/v0";

/// The canonical empty message.
const EMPTY_MESSAGE: &str = "google.protobuf.Empty";

/// An authored service document before descriptor resolution.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDocument {
    #[serde(default)]
    inputs: BTreeMap<String, InputDecl>,
    #[serde(default)]
    outputs: BTreeMap<String, OutputDecl>,
    #[serde(default)]
    operations: BTreeMap<String, ServedDecl>,
    #[serde(default)]
    calls: BTreeMap<String, CallDecl>,
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
    /// Resolves a capability document's message against the SDK-owned
    /// standard vocabulary: no owned schemas are compiled, so the type must
    /// be a framework-owned standard definition.
    fn resolve_standard(fqn: &str, owner: &str, path: &Path) -> Result<Self, Error> {
        let invalid = |message: String| Error::ApiInput {
            path: path.to_owned(),
            message: format!("{owner} references {fqn}: {message}"),
        };
        if fqn == EMPTY_MESSAGE {
            return Ok(Self {
                fqn: fqn.to_owned(),
                rust_path: "::phoxal::contracts::Empty".to_owned(),
            });
        }
        if let Some(rust_path) = crate::sdk_type_path(fqn) {
            return Ok(Self {
                fqn: fqn.to_owned(),
                rust_path,
            });
        }
        Err(invalid(
            "capability-generated endpoints reference only the SDK standard \
             vocabulary"
                .into(),
        ))
    }
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
    /// Request message.
    pub request: ResolvedMessage,
    /// Response message.
    pub response: ResolvedMessage,
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

/// A component.yaml's raw capabilities.
pub struct ComponentSurface {
    /// The component's raw capability declarations.
    pub capabilities: BTreeMap<String, serde_yaml::Value>,
}

pub fn parse_component_surface(source: &[u8], path: &Path) -> Result<ComponentSurface, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ComponentWire {
        schema: String,
        #[allow(
            dead_code,
            reason = "component DTO carrier; validated by the SDK component document"
        )]
        model: serde_yaml::Value,
        #[serde(default)]
        capabilities: BTreeMap<String, serde_yaml::Value>,
        #[serde(default)]
        #[allow(
            dead_code,
            reason = "component DTO carrier; validated by the SDK component document"
        )]
        assets: Vec<PathBuf>,
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
    Ok(ComponentSurface {
        capabilities: wire.capabilities,
    })
}

/// Standard capability policies: one authority per standard endpoint.
///
/// A velocity-commanded motor implies a leased actuator setpoint input; an
/// encoder implies a queued encoder sample output. Bounds and leases are
/// documented defaults until authored beside the capability.
const STANDARD_MOTOR_LEASE_MS: u64 = 100;
const STANDARD_MOTOR_MAX_BYTES: u64 = 1024;
const STANDARD_ENCODER_MAX_ITEMS: u64 = 16;
const STANDARD_ENCODER_MAX_BYTES: u64 = 8192;
const STANDARD_IMU_MAX_ITEMS: u64 = 16;
const STANDARD_IMU_MAX_BYTES: u64 = 16_384;
const STANDARD_ACCELEROMETER_MAX_BYTES: u64 = 8192;
const STANDARD_GYROSCOPE_MAX_BYTES: u64 = 8192;
const STANDARD_CAMERA_MAX_ITEMS: u64 = 4;
const STANDARD_CAMERA_MAX_BYTES: u64 = 8_388_608;
const STANDARD_RANGE_MAX_ITEMS: u64 = 16;
const STANDARD_RANGE_MAX_BYTES: u64 = 512;
const STANDARD_GNSS_MAX_ITEMS: u64 = 8;
const STANDARD_GNSS_MAX_BYTES: u64 = 8192;
const ACTUATOR_SETPOINT: &str = "phoxal.component.actuator.v1.ActuatorSetpoint";
const ENCODER_SAMPLE: &str = "phoxal.robotics.v1.EncoderSample";
const IMU_SAMPLE: &str = "phoxal.component.imu.v1.ImuSample";
const ACCELEROMETER_SAMPLE: &str = "phoxal.component.imu.v1.AccelerometerSample";
const GYROSCOPE_SAMPLE: &str = "phoxal.component.imu.v1.GyroscopeSample";
const CAMERA_FRAME: &str = "phoxal.component.camera.v1.CameraFrame";
const DEPTH_FRAME: &str = "phoxal.component.camera.v1.DepthFrame";
const RANGE_SAMPLE: &str = "phoxal.robotics.v1.RangeSample";
const GNSS_SAMPLE: &str = "phoxal.component.gnss.v1.GnssSample";

/// Derives the standard endpoint document implied by a component's
/// capabilities. Returns `None` when the component declares none.
pub fn capability_document(
    capabilities: &BTreeMap<String, serde_yaml::Value>,
    path: &Path,
) -> Result<Option<ServiceDocument>, Error> {
    let reject = |message: String| Error::ApiInput {
        path: path.to_owned(),
        message,
    };
    // The SDK's component document owns the full capability schema; only
    // the contract-relevant fields are read and the remaining physical
    // configuration is deliberately ignored here.
    #[derive(Deserialize)]
    struct MotorDecl {
        command: String,
    }
    #[derive(Deserialize)]
    struct CameraDecl {
        mode: String,
    }
    #[derive(Deserialize)]
    struct KindDecl {
        kind: String,
    }
    let queue_output = |outputs: &mut BTreeMap<String, OutputDecl>,
                        endpoint: &str,
                        message: &str,
                        max_items: u64,
                        max_bytes: u64|
     -> Result<(), Error> {
        if outputs
            .insert(
                endpoint.to_owned(),
                OutputDecl {
                    message: message.to_owned(),
                    delivery: Delivery::Queue,
                    retained_latest: false,
                    lease: None,
                    projection: None,
                    bootstrap: false,
                    on_change: false,
                    max_items: Some(max_items),
                    max_bytes: Some(max_bytes),
                },
            )
            .is_some()
        {
            return Err(reject(format!(
                "the standard `{endpoint}` endpoint is declared by more than one capability"
            )));
        }
        Ok(())
    };
    let mut inputs = BTreeMap::new();
    let mut outputs = BTreeMap::new();
    for (name, declaration) in capabilities {
        let kind: KindDecl = serde_yaml::from_value(declaration.clone())
            .map_err(|error| reject(format!("capability `{name}` is invalid: {error}")))?;
        match kind.kind.as_str() {
            "motor" => {
                let motor: MotorDecl = serde_yaml::from_value(declaration.clone())
                    .map_err(|error| reject(format!("capability `{name}` is invalid: {error}")))?;
                if name != "motor" {
                    return Err(reject(format!(
                        "capability `{name}` declares kind `motor`; the name and kind must agree"
                    )));
                }
                if motor.command != "velocity" {
                    return Err(reject(format!(
                        "capability `{name}` declares command {:?}; only `velocity` has a \
                         standard contract generation",
                        motor.command
                    )));
                }
                if inputs
                    .insert(
                        "actuator".to_owned(),
                        InputDecl {
                            message: ACTUATOR_SETPOINT.to_owned(),
                            delivery: Delivery::Latest,
                            required: true,
                            max_age_ms: None,
                            max_items: None,
                            max_bytes: Some(STANDARD_MOTOR_MAX_BYTES),
                            lease: Some(LeaseDecl {
                                valid_for_ms: STANDARD_MOTOR_LEASE_MS,
                            }),
                        },
                    )
                    .is_some()
                {
                    return Err(reject(
                        "the standard `actuator` endpoint is declared by more than one \
                         capability"
                            .to_owned(),
                    ));
                }
            }
            "encoder" | "imu" | "accelerometer" | "gyroscope" | "depth" | "range" | "gnss" => {
                if name != &kind.kind {
                    return Err(reject(format!(
                        "capability `{name}` declares kind {:?}; the name and kind must agree",
                        kind.kind
                    )));
                }
                let (message, max_items, max_bytes) = match kind.kind.as_str() {
                    "encoder" => (
                        ENCODER_SAMPLE,
                        STANDARD_ENCODER_MAX_ITEMS,
                        STANDARD_ENCODER_MAX_BYTES,
                    ),
                    "imu" => (IMU_SAMPLE, STANDARD_IMU_MAX_ITEMS, STANDARD_IMU_MAX_BYTES),
                    "accelerometer" => (
                        ACCELEROMETER_SAMPLE,
                        STANDARD_IMU_MAX_ITEMS,
                        STANDARD_ACCELEROMETER_MAX_BYTES,
                    ),
                    "gyroscope" => (
                        GYROSCOPE_SAMPLE,
                        STANDARD_IMU_MAX_ITEMS,
                        STANDARD_GYROSCOPE_MAX_BYTES,
                    ),
                    "depth" => (
                        DEPTH_FRAME,
                        STANDARD_CAMERA_MAX_ITEMS,
                        STANDARD_CAMERA_MAX_BYTES,
                    ),
                    "range" => (
                        RANGE_SAMPLE,
                        STANDARD_RANGE_MAX_ITEMS,
                        STANDARD_RANGE_MAX_BYTES,
                    ),
                    _ => (
                        GNSS_SAMPLE,
                        STANDARD_GNSS_MAX_ITEMS,
                        STANDARD_GNSS_MAX_BYTES,
                    ),
                };
                queue_output(&mut outputs, name, message, max_items, max_bytes)?;
            }
            "camera" => {
                let camera: CameraDecl = serde_yaml::from_value(declaration.clone())
                    .map_err(|error| reject(format!("capability `{name}` is invalid: {error}")))?;
                if camera.mode != "mono" && camera.mode != "rgb" {
                    return Err(reject(format!(
                        "camera capability `{name}` declares mode {:?}; only `mono` and `rgb` \
                         have a standard contract generation",
                        camera.mode
                    )));
                }
                queue_output(
                    &mut outputs,
                    name,
                    CAMERA_FRAME,
                    STANDARD_CAMERA_MAX_ITEMS,
                    STANDARD_CAMERA_MAX_BYTES,
                )?;
            }
            // A capability kind without a standard generation owns no
            // derived endpoints: its surface stays explicitly declared in
            // Rust until that kind gains a generation.
            _ => continue,
        }
    }
    if inputs.is_empty() && outputs.is_empty() {
        return Ok(None);
    }
    Ok(Some(ServiceDocument {
        inputs,
        outputs,
        operations: BTreeMap::new(),
        calls: BTreeMap::new(),
    }))
}

/// Resolves a capability-derived document against the SDK-owned standard
/// vocabulary: message types map to their owning crates and no owned
/// schemas are compiled.
pub fn resolve_standard_document(
    document: &ServiceDocument,
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
                "{owner}: optional capability inputs are not supported; \
                 every input requires a composition binding"
            )));
        }
        let message = ResolvedMessage::resolve_standard(&decl.message, &owner, path)?;
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
        let message = ResolvedMessage::resolve_standard(&decl.message, &owner, path)?;
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
        let resolved = resolve_exchange(name, &decl.contract, &decl.request, &decl.response, path)?;
        operations.push(ResolvedOperation {
            name: name.clone(),
            request: resolved.0,
            response: resolved.1,
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
                "call {name:?}: optional capability requirements are not supported; \
                 every call requires a composition binding"
            )));
        }
        let resolved = resolve_exchange(name, &decl.contract, &decl.request, &decl.response, path)?;
        calls.push(ResolvedOperation {
            name: name.clone(),
            request: resolved.0,
            response: resolved.1,
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
    let request = ResolvedMessage::resolve_standard(request, &owner, path)?;
    let response = ResolvedMessage::resolve_standard(response, &owner, path)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(yaml: &str) -> BTreeMap<String, serde_yaml::Value> {
        serde_yaml::from_str::<BTreeMap<String, serde_yaml::Value>>(yaml).expect("capability wire")
    }

    #[test]
    fn standard_capabilities_derive_their_endpoint_contracts() {
        let wire = capabilities(
            "{motor: {kind: motor, command: velocity, max_torque_nm: 2.0}, encoder: {kind: encoder, publish_rate_hz: 50.0}}",
        );
        let document = capability_document(&wire, Path::new("component.yaml")).expect("derivation");
        let document = document.expect("the component declares capabilities");
        let actuator = document
            .inputs
            .get("actuator")
            .expect("leased actuator input");
        assert_eq!(
            actuator.message,
            "phoxal.component.actuator.v1.ActuatorSetpoint"
        );
        assert_eq!(
            actuator
                .lease
                .as_ref()
                .expect("standard lease")
                .valid_for_ms,
            100
        );
        assert_eq!(actuator.max_bytes, Some(1024));
        let encoder = document
            .outputs
            .get("encoder")
            .expect("queued encoder output");
        assert_eq!(encoder.message, "phoxal.robotics.v1.EncoderSample");
        assert_eq!(encoder.max_items, Some(16));
        let resolved = resolve_standard_document(&document, Path::new("component.yaml"))
            .expect("standard resolution");
        assert_eq!(
            resolved.inputs[0].message.rust_path,
            "::phoxal::contracts::component::actuator::ActuatorSetpoint"
        );
        assert_eq!(
            resolved.outputs[0].message.rust_path,
            "::phoxal::contracts::component::encoder::EncoderSample"
        );
    }

    #[test]
    fn unsupported_capabilities_are_rejected_without_inference() {
        let torque = capabilities("{motor: {kind: motor, command: torque}}");
        let error = capability_document(&torque, Path::new("component.yaml"))
            .expect_err("torque command has no standard generation");
        assert!(error.to_string().contains("only `velocity`"));

        // Capabilities without a generation derive nothing: their surface
        // stays explicitly declared.
        let ungenerated = capabilities("{lidar: {kind: lidar}}");
        assert!(
            capability_document(&ungenerated, Path::new("component.yaml"))
                .expect("no derivation for ungenerated capabilities")
                .is_none()
        );

        let empty = capabilities("{}");
        assert!(
            capability_document(&empty, Path::new("component.yaml"))
                .expect("empty derivation")
                .is_none()
        );
    }
}
