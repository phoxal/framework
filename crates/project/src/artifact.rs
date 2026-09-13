//! Native artifact contract inspection and connection validation.
//!
//! This module reads linker sections from ELF and Mach-O files through the
//! object-file parser. It never executes an inspected binary and it does not
//! parse Protobuf source into a second schema model.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use object::{Object, ObjectSection};
use prost_reflect::DescriptorPool;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::document::RobotDocument;

const ARTIFACT_SECTION_NAMES: [&str; 2] = [".phoxal_art", "__phoxal_art"];
const DESCRIPTOR_SECTION_NAMES: [&str; 2] = [".phoxal_desc", "__phoxal_desc"];
const ARTIFACT_MAGIC: &[u8; 8] = b"PHXART0\n";
const DESCRIPTOR_MAGIC: &[u8; 8] = b"PHXDESC0";
const MAX_SECTION_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 65_536;
const MAX_RECORDS: usize = 64;
const MAX_DESCRIPTOR_FILES: usize = 1_024;

/// A native artifact contract extracted without executing the binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactContract {
    /// Runtime timing, configuration, and binding metadata.
    pub runtime: RuntimeRecord,
    /// Original descriptor closures embedded by generated contract owners.
    pub descriptors: Vec<DescriptorInfo>,
}

impl ArtifactContract {
    /// Returns a manifest-safe summary while retaining no native bytes.
    #[must_use]
    pub fn summary(&self) -> ArtifactSummary {
        ArtifactSummary {
            runtime: self.runtime.clone(),
            descriptors: self
                .descriptors
                .iter()
                .map(DescriptorInfo::summary)
                .collect(),
        }
    }
}

/// Manifest-safe artifact contract information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSummary {
    /// Runtime metadata and checked bindings.
    pub runtime: RuntimeRecord,
    /// Digest and descriptor-file inventory for each retained closure.
    pub descriptors: Vec<DescriptorSummary>,
}

/// One unchanged standard FileDescriptorSet retained by a contract owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorInfo {
    /// SHA-256 digest of the original encoded descriptor bytes.
    pub sha256: String,
    /// Exact descriptor-set byte count.
    pub bytes: u64,
    /// File names retained in the descriptor closure.
    pub files: Vec<String>,
    raw: Vec<u8>,
}

impl DescriptorInfo {
    fn summary(&self) -> DescriptorSummary {
        DescriptorSummary {
            sha256: self.sha256.clone(),
            bytes: self.bytes,
            files: self.files.clone(),
        }
    }
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

/// Why native artifact contract inspection or graph validation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input is not a supported native object file.
    #[error("cannot parse native artifact: {0}")]
    Object(String),
    /// A native contract section is larger than the bounded inspector input.
    #[error("native artifact section {section} is too large ({bytes} bytes)")]
    SectionTooLarge {
        /// Section being inspected.
        section: &'static str,
        /// Observed size.
        bytes: usize,
    },
    /// The binary did not carry the required runtime artifact record.
    #[error("native artifact has no phoxal runtime contract record")]
    MissingRecord,
    /// A framed record was malformed.
    #[error("malformed {kind} artifact frame: {message}")]
    MalformedFrame {
        /// Frame family.
        kind: &'static str,
        /// Specific structural issue.
        message: String,
    },
    /// A JSON record could not be decoded.
    #[error("invalid runtime artifact JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A retained descriptor set could not be decoded.
    #[error("invalid retained descriptor set: {0}")]
    Descriptor(#[from] prost_reflect::DescriptorError),
    /// A runtime record violated its own bounds or role checks.
    #[error("invalid runtime artifact contract: {0}")]
    InvalidContract(String),
    /// A connected endpoint could not be matched to a checked binding.
    #[error("invalid connected endpoint {consumer} <- {producer}: {message}")]
    InvalidConnection {
        /// Consumer endpoint.
        consumer: String,
        /// Producer endpoint.
        producer: String,
        /// Specific mismatch.
        message: String,
    },
}

/// Inspects one native binary without loading or executing it.
pub fn inspect_file(path: &Path) -> Result<ArtifactContract, Error> {
    let bytes = fs::read(path).map_err(|error| Error::Object(error.to_string()))?;
    inspect_bytes(&bytes)
}

/// Inspects native artifact sections from an in-memory ELF or Mach-O image.
pub fn inspect_bytes(bytes: &[u8]) -> Result<ArtifactContract, Error> {
    let file = object::File::parse(bytes).map_err(|error| Error::Object(error.to_string()))?;
    let mut artifact_sections = Vec::new();
    let mut descriptor_sections = Vec::new();
    for section in file.sections() {
        let name = section.name().unwrap_or_default();
        if ARTIFACT_SECTION_NAMES.contains(&name) {
            artifact_sections.push(
                section
                    .data()
                    .map_err(|error| Error::Object(error.to_string()))?,
            );
        } else if DESCRIPTOR_SECTION_NAMES.contains(&name) {
            descriptor_sections.push(
                section
                    .data()
                    .map_err(|error| Error::Object(error.to_string()))?,
            );
        }
    }
    let records = artifact_sections
        .iter()
        .try_fold(Vec::new(), |mut records, section| {
            if section.len() > MAX_SECTION_BYTES {
                return Err(Error::SectionTooLarge {
                    section: ".phoxal_art",
                    bytes: section.len(),
                });
            }
            records.extend(parse_artifact_records(section)?);
            Ok(records)
        })?;
    if records.len() != 1 {
        return Err(if records.is_empty() {
            Error::MissingRecord
        } else {
            Error::MalformedFrame {
                kind: "runtime",
                message: format!("expected one record, found {}", records.len()),
            }
        });
    }
    let runtime: RuntimeRecord = serde_json::from_slice(records[0])?;
    validate_runtime(&runtime)?;
    let descriptors =
        descriptor_sections
            .iter()
            .try_fold(Vec::new(), |mut descriptors, section| {
                if section.len() > MAX_SECTION_BYTES {
                    return Err(Error::SectionTooLarge {
                        section: ".phoxal_desc",
                        bytes: section.len(),
                    });
                }
                descriptors.extend(parse_descriptor_frames(section)?);
                Ok(descriptors)
            })?;
    let descriptors = descriptors
        .into_iter()
        .map(|raw| {
            let pool = DescriptorPool::decode(raw.as_slice())?;
            let files = pool
                .files()
                .map(|file| file.name().to_owned())
                .collect::<Vec<_>>();
            if files.len() > MAX_DESCRIPTOR_FILES {
                return Err(Error::InvalidContract(format!(
                    "descriptor closure contains {} files, limit is {MAX_DESCRIPTOR_FILES}",
                    files.len()
                )));
            }
            let mut hasher = Sha256::new();
            hasher.update(&raw);
            Ok(DescriptorInfo {
                sha256: format!("{:x}", hasher.finalize()),
                bytes: raw.len() as u64,
                files,
                raw,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(ArtifactContract {
        runtime,
        descriptors,
    })
}

fn parse_artifact_records(section: &[u8]) -> Result<Vec<&[u8]>, Error> {
    let mut records = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = section[cursor..]
        .windows(ARTIFACT_MAGIC.len())
        .position(|window| window == ARTIFACT_MAGIC)
    {
        let magic = cursor + relative;
        let length_start = magic + ARTIFACT_MAGIC.len();
        let payload_start = length_start + 4;
        if payload_start > section.len() {
            return Err(Error::MalformedFrame {
                kind: "runtime",
                message: "truncated length header".to_owned(),
            });
        }
        let length = u32::from_le_bytes(section[length_start..payload_start].try_into().map_err(
            |_| Error::MalformedFrame {
                kind: "runtime",
                message: "invalid length header".to_owned(),
            },
        )?) as usize;
        if length > MAX_RECORD_BYTES {
            return Err(Error::MalformedFrame {
                kind: "runtime",
                message: format!("record length {length} exceeds {MAX_RECORD_BYTES}"),
            });
        }
        let end = payload_start
            .checked_add(length)
            .ok_or_else(|| Error::MalformedFrame {
                kind: "runtime",
                message: "record length overflow".to_owned(),
            })?;
        if end > section.len() {
            return Err(Error::MalformedFrame {
                kind: "runtime",
                message: "record extends beyond its section".to_owned(),
            });
        }
        records.push(&section[payload_start..end]);
        if records.len() > MAX_RECORDS {
            return Err(Error::MalformedFrame {
                kind: "runtime",
                message: format!("record count exceeds {MAX_RECORDS}"),
            });
        }
        cursor = end;
    }
    Ok(records)
}

fn parse_descriptor_frames(section: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut frames = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = section[cursor..]
        .windows(DESCRIPTOR_MAGIC.len())
        .position(|window| window == DESCRIPTOR_MAGIC)
    {
        let magic = cursor + relative;
        let length_start = magic + DESCRIPTOR_MAGIC.len();
        let payload_start = length_start + 8;
        if payload_start > section.len() {
            return Err(Error::MalformedFrame {
                kind: "descriptor",
                message: "truncated length header".to_owned(),
            });
        }
        let length = u64::from_le_bytes(section[length_start..payload_start].try_into().map_err(
            |_| Error::MalformedFrame {
                kind: "descriptor",
                message: "invalid length header".to_owned(),
            },
        )?) as usize;
        if length > MAX_SECTION_BYTES {
            return Err(Error::MalformedFrame {
                kind: "descriptor",
                message: format!("descriptor length {length} exceeds {MAX_SECTION_BYTES}"),
            });
        }
        let end = payload_start
            .checked_add(length)
            .ok_or_else(|| Error::MalformedFrame {
                kind: "descriptor",
                message: "descriptor length overflow".to_owned(),
            })?;
        if end > section.len() {
            return Err(Error::MalformedFrame {
                kind: "descriptor",
                message: "descriptor frame extends beyond its section".to_owned(),
            });
        }
        let payload = section[payload_start..end].to_vec();
        if !frames.iter().any(|existing| existing == &payload) {
            frames.push(payload);
        }
        cursor = end;
    }
    Ok(frames)
}

fn validate_runtime(runtime: &RuntimeRecord) -> Result<(), Error> {
    if runtime.schema != "phoxal/artifact/v0" {
        return Err(Error::InvalidContract(format!(
            "schema '{}' is not phoxal/artifact/v0",
            runtime.schema
        )));
    }
    if runtime.record != "runtime" {
        return Err(Error::InvalidContract(format!(
            "record '{}' is not runtime",
            runtime.record
        )));
    }
    if runtime.period_ms == 0 || runtime.timeout_ms == 0 || runtime.init_timeout_ms == 0 {
        return Err(Error::InvalidContract(
            "period and deadlines must be positive".to_owned(),
        ));
    }
    if !runtime.config_schema.is_object() {
        return Err(Error::InvalidContract(
            "config_schema must be a JSON schema object".to_owned(),
        ));
    }
    let mut input_names = BTreeSet::new();
    for input in &runtime.inputs {
        if !input_names.insert(input.name.as_str()) {
            return Err(Error::InvalidContract(format!(
                "duplicate input binding '{}'",
                input.name
            )));
        }
        let requires_request = matches!(
            input.kind,
            InputKind::Read | InputKind::Request | InputKind::Commands
        );
        let requires_response = input.kind != InputKind::Operation;
        if input.request_fqn.is_some() != requires_request
            || input.response_fqn.is_some() != requires_response
            || input
                .request_fqn
                .iter()
                .chain(input.response_fqn.iter())
                .any(|name| name.is_empty() || !name.is_ascii())
        {
            return Err(Error::InvalidContract(format!(
                "input '{}' must retain its concrete generated Protobuf message identities",
                input.name
            )));
        }
        if input.port.is_some() != input.signature.is_some() {
            return Err(Error::InvalidContract(format!(
                "input '{}' must retain port and signature together",
                input.name
            )));
        }
        if let Some(signature) = &input.signature {
            if input.port.as_deref() != Some(signature.name.as_str()) {
                return Err(Error::InvalidContract(format!(
                    "input '{}' port name does not match its signature",
                    input.name
                )));
            }
            if input.kind == InputKind::Commands && signature.kind != PortKind::Commands {
                return Err(Error::InvalidContract(format!(
                    "Commands input '{}' is bound to {:?}",
                    input.name, signature.kind
                )));
            }
        }
    }
    let mut output_names = BTreeSet::new();
    for output in runtime
        .transient_outputs
        .iter()
        .chain(runtime.service_outputs.iter())
    {
        if !output_names.insert(output.name.as_str()) {
            return Err(Error::InvalidContract(format!(
                "duplicate output binding '{}'",
                output.name
            )));
        }
        if output.port.is_some() != output.signature.is_some() {
            return Err(Error::InvalidContract(format!(
                "output '{}' must retain port and signature together",
                output.name
            )));
        }
        if output.port.is_some() && output.max_bytes.is_none_or(|bound| bound == 0) {
            return Err(Error::InvalidContract(format!(
                "served output '{}' has no positive response/publication byte bound",
                output.name
            )));
        }
        if output.kind == OutputKind::Read
            && output.max_request_bytes.is_none_or(|bound| bound == 0)
        {
            return Err(Error::InvalidContract(format!(
                "read output '{}' has no positive request byte bound",
                output.name
            )));
        }
        if let Some(signature) = &output.signature {
            if output.port.as_deref() != Some(signature.name.as_str()) {
                return Err(Error::InvalidContract(format!(
                    "output '{}' port name does not match its signature",
                    output.name
                )));
            }
            let expected = match output.kind {
                OutputKind::State => Some(PortKind::State),
                OutputKind::Sample => Some(PortKind::Sample),
                OutputKind::Event => Some(PortKind::Event),
                OutputKind::Stream => Some(PortKind::Stream),
                OutputKind::Setpoint => Some(PortKind::Setpoint),
                OutputKind::Read => Some(PortKind::Read),
                OutputKind::Reply | OutputKind::Activate | OutputKind::Operation => None,
            };
            if expected != Some(signature.kind) {
                return Err(Error::InvalidContract(format!(
                    "output '{}' role {:?} does not match {:?}",
                    output.name, output.kind, signature.kind
                )));
            }
        }
    }
    Ok(())
}

/// Validates all graph connections for which both endpoint artifacts exist.
///
/// The validation is intentionally endpoint-first: kind and complete request /
/// response identities are compared before any descriptor message-root
/// filtering can remove service evidence.
pub use connections::validate_connected_endpoints;

mod connections;

#[cfg(test)]
mod tests {
    use object::write::Object;
    use object::{Architecture, BinaryFormat, Endianness, SectionKind};

    use super::*;

    fn frame(json: &str) -> Vec<u8> {
        let mut bytes = ARTIFACT_MAGIC.to_vec();
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(json.as_bytes());
        bytes
    }

    fn native_artifact(json: &str, section_name: &[u8]) -> Vec<u8> {
        let mut object = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
        let section =
            object.add_section(Vec::new(), section_name.to_vec(), SectionKind::ReadOnlyData);
        object.append_section_data(section, &frame(json), 1);
        object.write().expect("synthetic object")
    }

    const EMPTY_RUNTIME: &str = r#"{
        "schema":"phoxal/artifact/v0",
        "record":"runtime",
        "period_ms":20,
        "timeout_ms":100,
        "init_timeout_ms":1000,
        "config_schema":{"type":"object"},
        "inputs":[],
        "transient_outputs":[],
        "service_outputs":[]
    }"#;

    #[test]
    fn extracts_a_record_from_an_elf_section_without_execution() {
        let bytes = native_artifact(EMPTY_RUNTIME, b".phoxal_art");
        let contract = inspect_bytes(&bytes).expect("native artifact contract");
        assert_eq!(contract.runtime.period_ms, 20);
        assert!(contract.descriptors.is_empty());
    }

    #[test]
    fn rejects_an_oversized_record_before_json_parsing() {
        let mut section = ARTIFACT_MAGIC.to_vec();
        section.extend_from_slice(&((MAX_RECORD_BYTES + 1) as u32).to_le_bytes());
        section.resize(section.len() + MAX_RECORD_BYTES + 1, b'x');
        let error = parse_artifact_records(&section).expect_err("bounded parser must reject");
        assert!(matches!(
            error,
            Error::MalformedFrame {
                kind: "runtime",
                ..
            }
        ));
    }

    #[test]
    fn rejects_unknown_port_kinds_in_artifact_json() {
        let json = EMPTY_RUNTIME.replace(
            "\"service_outputs\":[]\n    }",
            "\"service_outputs\":[{\"name\":\"status\",\"kind\":\"future\",\"port\":null,\"signature\":null,\"input\":null,\"project\":null,\"max_items\":null,\"max_bytes\":null,\"max_request_bytes\":null,\"every_steps\":null,\"on_change\":false,\"bootstrap\":false,\"valid_for_ms\":null,\"timeout_ms\":null,\"cancel_grace_ms\":null}]\n    }",
        );
        let bytes = native_artifact(&json, b".phoxal_art");
        assert!(matches!(inspect_bytes(&bytes), Err(Error::Json(_))));
    }

    #[test]
    fn rejects_kind_only_incompatibility_before_payload_filtering() {
        let document = RobotDocument::parse(
            r#"
robot:
  id: rover
  components: {}
services:
  consumer: {}
  producer: {}
connections:
  consumer.input: producer.output
"#,
        )
        .expect("document parses");
        let signature = PortSignature {
            name: "output".to_owned(),
            service: "example.Service".to_owned(),
            method: "Output".to_owned(),
            kind: PortKind::Event,
            request: "google.protobuf.Empty".to_owned(),
            response: "example.Payload".to_owned(),
        };
        let producer = ArtifactContract {
            runtime: RuntimeRecord {
                schema: "phoxal/artifact/v0".to_owned(),
                record: "runtime".to_owned(),
                period_ms: 1,
                timeout_ms: 1,
                init_timeout_ms: 1,
                config_schema: serde_json::json!({"type":"object"}),
                inputs: Vec::new(),
                transient_outputs: Vec::new(),
                service_outputs: vec![OutputRecord {
                    name: "output".to_owned(),
                    kind: OutputKind::Event,
                    port: Some("output".to_owned()),
                    signature: Some(signature),
                    input: None,
                    project: None,
                    max_items: None,
                    max_bytes: None,
                    max_request_bytes: None,
                    every_steps: None,
                    on_change: false,
                    bootstrap: false,
                    valid_for_ms: None,
                    timeout_ms: None,
                    cancel_grace_ms: None,
                }],
            },
            descriptors: Vec::new(),
        };
        let consumer = ArtifactContract {
            runtime: RuntimeRecord {
                schema: "phoxal/artifact/v0".to_owned(),
                record: "runtime".to_owned(),
                period_ms: 1,
                timeout_ms: 1,
                init_timeout_ms: 1,
                config_schema: serde_json::json!({"type":"object"}),
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
                service_outputs: Vec::new(),
            },
            descriptors: Vec::new(),
        };
        let contracts = BTreeMap::from([
            ("consumer".to_owned(), consumer),
            ("producer".to_owned(), producer),
        ]);
        let error = validate_connected_endpoints(&document, &contracts)
            .expect_err("event cannot satisfy a latest state input");
        assert!(error.to_string().contains("requires Some(State)"));
    }
}
