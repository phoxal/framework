//! Prepared Rust-contract products and the client generation from them.
//!
//! `cargo phoxal prepare` builds selected participant artifacts, extracts
//! their compiled contracts (endpoint metadata plus assembled descriptor
//! closures), and writes them under the robot's `.phoxal/` tree. This
//! module reads those exact products so a composing brain generates its
//! external instance bindings from extracted artifacts — never from the
//! participant's source tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost::Message;
use prost_types::FileDescriptorSet;
use serde::{Deserialize, Serialize};

use crate::Error;

/// Narrow read-model of one retained input record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedInput {
    #[serde(default)]
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    request_fqn: Option<String>,
    #[serde(default)]
    response_fqn: Option<String>,
    #[serde(default)]
    signature: Option<PreparedSignature>,
}

/// Narrow read-model of one retained output record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedOutput {
    #[serde(default)]
    signature: Option<PreparedSignature>,
}

/// Narrow read-model of one retained method signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedSignature {
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub shape: String,
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub retained_latest: bool,
    #[serde(default)]
    pub lease_valid_for_ms: Option<u64>,
}

/// Narrow read-model of one retained runtime record.
///
/// The authoritative model is `phoxal::artifact`; this read-model carries
/// only the fields client generation consumes, deserialized from the same
/// retained JSON bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedRuntime {
    #[serde(default)]
    inputs: Vec<PreparedInput>,
    #[serde(default)]
    transient_outputs: Vec<PreparedOutput>,
    #[serde(default)]
    service_outputs: Vec<PreparedOutput>,
}

/// The prepared product files of one participant contract. One
/// directory under `.phoxal/prepared/` carries both files as a single
/// replaceable product: `contract.json` owns the runtime metadata, the
/// complete selection identity, and the executable digest the products
/// were extracted from; `descriptors.pb` is the standard Protobuf
/// `FileDescriptorSet` (not a Phoxal schema format).
pub const DESCRIPTORS_FILE: &str = "descriptors.pb";
pub const CONTRACT_FILE: &str = "contract.json";

/// The prepared-contract layout generation. Readers reject products
/// recorded under any other generation instead of guessing at a
/// different shape.
pub const CONTRACT_GENERATION: u32 = 1;

/// The root directory holding every prepared contract of one project.
pub const PREPARED_ROOT: &str = ".phoxal/prepared";

/// The directory key of a package's own self-prepared contract, written
/// by `cargo phoxal prepare` run inside a standalone service package.
pub const SELF_KEY: &str = "self";

/// The complete identity of one selection, recorded inside
/// `contract.json` so a shortened key component can always be validated
/// against the full identity it abbreviates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreparedSelection {
    /// A robot-local relative path source.
    Path {
        /// The authored relative path, verbatim.
        path: String,
    },
    /// A registry package at an exact version.
    Registry {
        /// Registry short name (`phoxal`).
        registry: String,
        /// Package name.
        name: String,
        /// Exact version.
        version: String,
    },
    /// A Git repository pinned to a full commit.
    Git {
        /// Repository short name.
        name: String,
        /// The full 40-character commit.
        revision: String,
    },
    /// The package preparing its own default binary (`cargo phoxal
    /// prepare` inside a standalone service package).
    SelfHosted {
        /// The preparing package's name.
        package: String,
    },
}

/// The executable a prepared contract was extracted from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedExecutable {
    /// SHA-256 of the exact executable bytes.
    pub sha256: String,
    /// The package the executable was built from.
    pub package: String,
    /// The package version, when the source carries one.
    pub version: Option<String>,
}

/// The `contract.json` envelope: one coherent record of what this
/// product is, what it was extracted from, and the runtime metadata
/// itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedContractFile {
    /// The layout generation this product belongs to.
    pub generation: u32,
    /// The complete selection identity.
    pub selection: PreparedSelection,
    /// The explicit `binary:` key of the selection, when one was set.
    pub binary: Option<String>,
    /// The executable the products were extracted from.
    pub executable: PreparedExecutable,
    /// The retained runtime record, verbatim, so consumers observe the
    /// exact artifact metadata rather than a lossy re-encoding.
    pub runtime: serde_json::Value,
}

/// Sanitizes one readable key segment: lowercase letters, digits, and
/// dashes survive; everything else folds to `-` runs.
fn readable_segment(text: &str) -> String {
    // Dots survive: they are filesystem-safe everywhere and folding
    // them into dashes would collide distinct versions (`0.1.0` and
    // `0-1-0`).
    let mut segment = String::with_capacity(text.len());
    let mut previous_dash = false;
    for character in text.chars() {
        let folded = if character.is_ascii_alphanumeric() || character == '.' {
            character.to_ascii_lowercase().to_string()
        } else {
            "-".to_owned()
        };
        if folded == "-" {
            if !previous_dash && !segment.is_empty() {
                segment.push('-');
            }
            previous_dash = true;
        } else {
            segment.push_str(&folded);
            previous_dash = false;
        }
    }
    // Edge dots and dashes fold away: a leading `..` from a relative
    // path must never reach the directory name, and the trailing digest
    // already guarantees distinctness.
    segment
        .trim_matches(|c: char| c == '-' || c == '.')
        .to_owned()
}

/// A twelve-hex digest of the COMPLETE selection identity — the
/// structured selection plus the exact binary key — so distinct
/// identities that fold to the same readable slug (including binary
/// names that differ only in folded characters, an absent versus an
/// explicit `default` binary, registry versions whose separators fold
/// together, and Git revisions sharing a twelve-character prefix)
/// always occupy distinct directories, and the recorded identity can
/// always be validated against it.
fn identity_digest(selection: &PreparedSelection, binary: Option<&str>) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    // Unit separators make the canonical text unambiguous without
    // escaping any field content; the binary marker distinguishes an
    // absent key from an explicit `default`.
    hasher.update(b"phoxal-prepared-v1\x1f");
    match selection {
        PreparedSelection::Path { path } => {
            hasher.update(b"path\x1f");
            hasher.update(path.as_bytes());
        }
        PreparedSelection::Registry {
            registry,
            name,
            version,
        } => {
            hasher.update(b"registry\x1f");
            hasher.update(registry.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(name.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(version.as_bytes());
        }
        PreparedSelection::Git { name, revision } => {
            hasher.update(b"git\x1f");
            hasher.update(name.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(revision.as_bytes());
        }
        PreparedSelection::SelfHosted { package } => {
            hasher.update(b"self\x1f");
            hasher.update(package.as_bytes());
        }
    }
    hasher.update(b"\x1f");
    match binary {
        Some(name) => {
            hasher.update(b"bin\x1f");
            hasher.update(name.as_bytes());
        }
        None => hasher.update(b"default"),
    }
    let digest = hasher.finalize();
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The readable, injective directory key of one complete selection.
///
/// The slug is the primary readable interface (`registry-<name>@<version>`,
/// `git-<name>@<rev12>`, `path-<slug>`), and the trailing digest of the
/// complete structured identity — selection plus exact binary key —
/// guarantees distinctness for everything the readable fold merges.
#[must_use]
pub fn prepared_key(selection: &PreparedSelection, binary: Option<&str>) -> String {
    let digest = identity_digest(selection, binary);
    let binary_suffix = match binary {
        Some(name) => format!("--bin-{}", readable_segment(name)),
        None => "--bin-default".to_owned(),
    };
    match selection {
        PreparedSelection::Path { path } => {
            let slug = readable_segment(&path.replace('/', "-"));
            format!(
                "path-{}-{}{}",
                if slug.is_empty() {
                    "root".to_owned()
                } else {
                    slug
                },
                digest,
                binary_suffix
            )
        }
        PreparedSelection::Registry {
            registry,
            name,
            version,
        } => format!(
            "registry-{}-{}@{}-{}{}",
            readable_segment(registry),
            readable_segment(name),
            readable_segment(version),
            digest,
            binary_suffix
        ),
        PreparedSelection::Git { name, revision } => format!(
            "git-{}@{}-{}{}",
            readable_segment(name),
            &revision[..revision.len().min(12)],
            digest,
            binary_suffix
        ),
        PreparedSelection::SelfHosted { .. } => SELF_KEY.to_owned(),
    }
}

/// The prepared-contract directory of one selection under a project's
/// `.phoxal/prepared/` root. Equivalent selections resolve to one
/// directory, so repeated selections prepare once.
#[must_use]
pub fn prepared_dir(
    project_root: &Path,
    selection: &PreparedSelection,
    binary: Option<&str>,
) -> PathBuf {
    project_root
        .join(PREPARED_ROOT)
        .join(prepared_key(selection, binary))
}

/// The prepared-contract directory of a package's own default binary,
/// written by `cargo phoxal prepare` run inside a standalone service
/// package. Its build helper turns these products into local client
/// bindings without compiling the package recursively.
#[must_use]
pub fn self_prepared_dir(package: &Path) -> PathBuf {
    package.join(PREPARED_ROOT).join(SELF_KEY)
}

/// Reads one prepared contract and requires that its recorded complete
/// identity — selection and binary key — equals the identity the caller
/// resolved the directory for. A directory whose readable slug or
/// folded binary suffix collides with another selection can never be
/// consumed on that selection's behalf.
pub fn read_prepared_for(
    contract_dir: &Path,
    selection: &PreparedSelection,
    binary: Option<&str>,
) -> Result<PreparedContract, Error> {
    let contract = read_prepared(contract_dir)?;
    validate_requested_identity(contract_dir, &contract.file, selection, binary)?;
    Ok(contract)
}

/// Compares a prepared file's recorded complete identity against the
/// identity the caller resolved the directory for. Structured
/// comparison only — readable folds never decide identity.
pub fn validate_requested_identity(
    contract_dir: &Path,
    file: &PreparedContractFile,
    selection: &PreparedSelection,
    binary: Option<&str>,
) -> Result<(), Error> {
    if file.selection != *selection || file.binary.as_deref() != binary {
        return Err(Error::ApiInput {
            path: contract_dir.to_owned(),
            message: format!(
                "prepared contract at `{}` records a different selection than the one requested ({:?} vs {selection:?}, binary {:?} vs {binary:?})",
                contract_dir.display(),
                file.selection,
                file.binary,
            ),
        });
    }
    Ok(())
}

/// Validates that a prepared-contract directory's key matches its
/// recorded complete identity — the shortened digest inside the key is
/// never trusted on its own.
pub fn validate_prepared_key(
    contract_dir: &Path,
    file: &PreparedContractFile,
) -> Result<(), Error> {
    let recorded = prepared_key(&file.selection, file.binary.as_deref());
    let actual = contract_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if actual != recorded {
        return Err(Error::ApiInput {
            path: contract_dir.to_owned(),
            message: format!(
                "prepared contract directory `{actual}` does not match its recorded identity (expected `{recorded}`)"
            ),
        });
    }
    Ok(())
}

/// One participant's prepared contract as the build helper consumes it.
#[derive(Debug, Clone)]
pub struct PreparedContract {
    /// The complete `contract.json` envelope.
    pub file: PreparedContractFile,
    /// The endpoint and policy metadata parsed from the verbatim
    /// record; the narrow read-model guiding generation.
    runtime: PreparedRuntime,
    /// The assembled standard descriptor closure.
    pub descriptors: FileDescriptorSet,
}

impl PreparedContract {
    /// The endpoint and policy metadata retained from the artifact.
    #[must_use]
    pub fn runtime(&self) -> &PreparedRuntime {
        &self.runtime
    }
}

/// Reads one prepared contract directory: the `contract.json` envelope
/// and the `descriptors.pb` closure as one coherent product, rejecting
/// foreign layout generations and directories whose key does not match
/// the recorded complete identity.
pub fn read_prepared(contract_dir: &Path) -> Result<PreparedContract, Error> {
    let contract_path = contract_dir.join(CONTRACT_FILE);
    let file: PreparedContractFile =
        serde_json::from_slice(&fs::read(&contract_path).map_err(|source| Error::Path {
            path: contract_path.clone(),
            source,
        })?)
        .map_err(|error| Error::ApiInput {
            path: contract_path.clone(),
            message: format!("prepared contract metadata is invalid: {error}"),
        })?;
    if file.generation != CONTRACT_GENERATION {
        return Err(Error::ApiInput {
            path: contract_path,
            message: format!(
                "prepared contract generation {} is not supported (expected {CONTRACT_GENERATION}); re-run `cargo phoxal prepare`",
                file.generation
            ),
        });
    }
    validate_prepared_key(contract_dir, &file)?;
    let runtime: PreparedRuntime =
        serde_json::from_value(file.runtime.clone()).map_err(|error| Error::ApiInput {
            path: contract_path.clone(),
            message: format!("prepared runtime metadata is invalid: {error}"),
        })?;
    let descriptors_path = contract_dir.join(DESCRIPTORS_FILE);
    let descriptors = FileDescriptorSet::decode(
        fs::read(&descriptors_path)
            .map_err(|source| Error::Path {
                path: descriptors_path.clone(),
                source,
            })?
            .as_slice(),
    )
    .map_err(|error| Error::ApiInput {
        path: contract_dir.to_owned(),
        message: format!("prepared descriptors are invalid: {error}"),
    })?;
    Ok(PreparedContract {
        file,
        runtime,
        descriptors,
    })
}

/// The Rust module path segments a Protobuf package maps to.
fn package_modules(package: &str) -> Result<Vec<String>, Error> {
    package
        .split('.')
        .map(|segment| {
            if segment.is_empty()
                || !segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(Error::ApiInput {
                    path: PathBuf::new(),
                    message: format!("invalid Protobuf package `{package}`"),
                });
            }
            Ok(segment.to_snake_case())
        })
        .collect()
}

/// One external endpoint a composed brain or client can bind.
pub enum PreparedEndpoint {
    /// An observation source.
    Observation {
        /// Endpoint name.
        name: String,
        /// Fully-qualified response message.
        response: String,
        /// Leased authority interval, if any.
        lease_valid_for_ms: Option<u64>,
    },
    /// A callable operation.
    Call {
        /// Endpoint name.
        name: String,
        /// Fully-qualified request message.
        request: String,
        /// Fully-qualified response message.
        response: String,
        /// Leased authority interval, if any.
        lease_valid_for_ms: Option<u64>,
    },
}

impl PreparedContract {
    /// Normalizes the retained records into brain-bindable endpoints.
    pub fn endpoints(&self) -> Vec<PreparedEndpoint> {
        let mut endpoints = Vec::new();
        for output in self
            .runtime
            .transient_outputs
            .iter()
            .chain(&self.runtime.service_outputs)
        {
            let Some(signature) = &output.signature else {
                continue;
            };
            if signature.shape == "call" {
                // A method-role leased output binds as a leased call whose
                // request is the published payload.
                endpoints.push(PreparedEndpoint::Call {
                    name: signature.endpoint.clone(),
                    request: signature.request.clone(),
                    response: signature.response.clone(),
                    lease_valid_for_ms: signature.lease_valid_for_ms,
                });
            } else {
                endpoints.push(PreparedEndpoint::Observation {
                    name: signature.endpoint.clone(),
                    response: signature.response.clone(),
                    lease_valid_for_ms: signature.lease_valid_for_ms,
                });
            }
        }
        for input in &self.runtime.inputs {
            let Some(signature) = &input.signature else {
                continue;
            };
            if input.role == "leased_value" {
                // A replaceable leased input binds as a leased call whose
                // request is the leased payload.
                endpoints.push(PreparedEndpoint::Call {
                    name: signature.endpoint.clone(),
                    request: signature.request.clone(),
                    response: signature.response.clone(),
                    lease_valid_for_ms: signature.lease_valid_for_ms,
                });
            } else if input.role == "call_ingress" {
                // A served operation binds as a plain call.
                endpoints.push(PreparedEndpoint::Call {
                    name: signature.endpoint.clone(),
                    request: signature.request.clone(),
                    response: signature.response.clone(),
                    lease_valid_for_ms: signature.lease_valid_for_ms,
                });
            }
        }
        endpoints
    }

    /// The response identity of one latest input, when it is a plain
    /// (unleased) latest input.
    pub fn input_response(&self, endpoint: &str) -> Option<String> {
        self.runtime.inputs.iter().find_map(|input| {
            if input.name != endpoint || input.role != "observation_latest" {
                return None;
            }
            input.response_fqn.clone()
        })
    }

    /// The response identity of one served data output.
    pub fn output_response(&self, endpoint: &str) -> Option<String> {
        self.runtime
            .transient_outputs
            .iter()
            .chain(&self.runtime.service_outputs)
            .find_map(|output| {
                output
                    .signature
                    .as_ref()
                    .filter(|signature| signature.endpoint == endpoint)
                    .map(|signature| signature.response.clone())
            })
    }

    /// The serialized descriptor closure, as prepared.
    pub fn descriptor_bytes(&self) -> Vec<u8> {
        use prost::Message as _;
        self.descriptors.encode_to_vec()
    }

    /// The call-shaped endpoint signatures this contract provides: the
    /// operations it serves on its call ingress, which a composed brain
    /// may name as provider descriptor markers.
    pub fn runtime_call_signatures(&self) -> impl Iterator<Item = &PreparedSignature> {
        self.runtime
            .inputs
            .iter()
            .filter(|input| input.role == "call_ingress")
            .filter_map(|input| input.signature.as_ref())
            .filter(|signature| signature.shape == "call")
    }

    fn signatures(&self) -> impl Iterator<Item = (&'static str, &PreparedSignature)> {
        self.runtime
            .transient_outputs
            .iter()
            .chain(&self.runtime.service_outputs)
            .filter_map(|output| {
                output
                    .signature
                    .as_ref()
                    .map(|signature| ("method", signature))
            })
            .chain(self.runtime.inputs.iter().filter_map(|input| {
                input
                    .signature
                    .as_ref()
                    .map(|signature| ("input", signature))
            }))
    }
}

/// Resolves the Rust path of one message through the descriptor closure.
pub fn rust_message_path(
    pool: &prost_reflect::DescriptorPool,
    fqn: &str,
    module_root: &str,
) -> Result<String, Error> {
    if let Some(path) = crate::sdk_type_path(fqn) {
        return Ok(path);
    }
    // A payload may be a message or an enumeration: both generate typed
    // Rust items in the same package modules.
    let (file_package, item_name) = if let Some(message) = pool.get_message_by_name(fqn) {
        (
            message.parent_file().package_name().to_owned(),
            message.name().to_owned(),
        )
    } else if let Some(enumeration) = pool.get_enum_by_name(fqn) {
        (
            enumeration.parent_file().package_name().to_owned(),
            enumeration.name().to_owned(),
        )
    } else {
        return Err(Error::ApiInput {
            path: PathBuf::new(),
            message: format!("prepared closure does not define `{fqn}`"),
        });
    };
    let mut segments = vec![module_root.to_owned()];
    segments.extend(package_modules(&file_package)?);
    segments.push(item_name);
    Ok(segments.join("::"))
}

/// Generates the composed client module for one bound instance from its
/// prepared contract.
pub fn emit_instance_module(
    instance: &str,
    prepared: &PreparedContract,
    pool: &prost_reflect::DescriptorPool,
    types_root: &str,
    methods_root: &str,
) -> Result<String, Error> {
    let module = instance.to_snake_case();
    if [
        "types",
        "calls",
        "projections",
        "service_methods",
        "operations",
    ]
    .contains(&module.as_str())
    {
        return Err(Error::ApiInput {
            path: PathBuf::from("robot.yaml"),
            message: format!(
                "API instance `{instance}` collides with the fixed generated module `{module}`"
            ),
        });
    }
    let mut output = String::new();
    let mut message_types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut endpoint_lines = Vec::new();
    for input in &prepared.runtime.inputs {
        for fqn in [input.request_fqn.as_deref(), input.response_fqn.as_deref()]
            .into_iter()
            .flatten()
            .filter(|fqn| *fqn != "google.protobuf.Empty")
        {
            let path = rust_message_path(pool, fqn, types_root)?;
            message_types
                .entry(fqn.rsplit('.').next().unwrap_or(fqn).to_owned())
                .or_default()
                .insert(path);
        }
    }
    for endpoint in prepared.endpoints() {
        match endpoint {
            PreparedEndpoint::Observation {
                name,
                response,
                lease_valid_for_ms: _,
            } => {
                let constant = name.to_shouty_snake_case();
                let response_path = rust_message_path(pool, &response, types_root)?;
                message_types
                    .entry(response.rsplit('.').next().unwrap_or(&response).to_owned())
                    .or_default()
                    .insert(response_path.clone());
                endpoint_lines.push(format!(
                    "    /// The typed contract method behind [`{name}`].\n    pub const {constant}: ::phoxal::contracts::ObservationMethod<{response_path}> = {methods_root}::{constant};\n    #[must_use]\n    pub fn {name}() -> ::phoxal::contracts::Observation<{response_path}> {{\n        {methods_root}::{constant}.bind({instance:?})\n    }}\n",
                ));
            }
            PreparedEndpoint::Call {
                name,
                request,
                response,
                lease_valid_for_ms,
            } => {
                let constant = name.to_shouty_snake_case();
                let request_path = rust_message_path(pool, &request, types_root)?;
                let response_path = rust_message_path(pool, &response, types_root)?;
                for (short, path) in [
                    (
                        request.rsplit('.').next().unwrap_or(&request),
                        &request_path,
                    ),
                    (
                        response.rsplit('.').next().unwrap_or(&response),
                        &response_path,
                    ),
                ] {
                    if short != "Empty" {
                        message_types
                            .entry(short.to_owned())
                            .or_default()
                            .insert(path.clone());
                    }
                }
                let lease =
                    lease_valid_for_ms.map_or_else(String::new, |value| format!("Some({value})"));
                endpoint_lines.push(format!(
                    "    /// The typed contract method behind [`{name}`].\n    pub const {constant}: ::phoxal::contracts::CallMethod<{request_path}, {response_path}> = {methods_root}::{constant};\n    #[must_use]\n    pub fn {name}(request: {request_path}) -> ::phoxal::contracts::Call<{request_path}, {response_path}> {{\n        {methods_root}::{constant}.bind({instance:?}, request)\n    }}\n",
                ));
                if !lease.is_empty() {
                    endpoint_lines.push(format!(
                        "    #[must_use]\n    pub fn withdraw_{name}() -> ::phoxal::contracts::Withdraw<{request_path}, {response_path}> {{\n        {methods_root}::{constant}.withdraw({instance:?})\n    }}\n",
                    ));
                }
            }
        }
    }
    output.push_str(&format!("pub mod {module} {{\n"));
    for paths in message_types.values() {
        if let Some(path) = (paths.len() == 1).then(|| paths.iter().next()).flatten() {
            output.push_str(&format!("    pub use {path};\n"));
        }
    }
    for line in endpoint_lines {
        output.push_str(&line);
    }
    output.push_str("}\n");
    Ok(output)
}

/// Generates the endpoint method constants module for one prepared contract.
pub fn emit_prepared_methods(
    prepared: &PreparedContract,
    pool: &prost_reflect::DescriptorPool,
    types_root: &str,
) -> Result<String, Error> {
    let mut output = String::from("// @generated by phoxal-build; do not edit.\n");
    for (kind, signature) in prepared.signatures() {
        let constant = signature.endpoint.to_shouty_snake_case();
        let lease = signature
            .lease_valid_for_ms
            .map_or_else(|| "None".to_owned(), |value| format!("Some({value})"));
        if kind == "method" && signature.shape == "observation" {
            let response_path = rust_message_path(pool, &signature.response, types_root)?;
            output.push_str(&format!(
                "pub const {constant}: ::phoxal::contracts::ObservationMethod<{response_path}> = ::phoxal::contracts::ObservationMethod::new({:?}, {:?}, {:?}, \"google.protobuf.Empty\", {:?}, {}, {}, &[]);\n",
                signature.service,
                signature.endpoint,
                signature.endpoint,
                signature.response,
                signature.retained_latest,
                lease,
            ));
        } else if signature.shape == "call" {
            let request_path = rust_message_path(pool, &signature.request, types_root)?;
            let response_path = rust_message_path(pool, &signature.response, types_root)?;
            let lease_literal = if signature.lease_valid_for_ms.is_some() {
                lease
            } else {
                "None".to_owned()
            };
            output.push_str(&format!(
                "pub const {constant}: ::phoxal::contracts::CallMethod<{request_path}, {response_path}> = ::phoxal::contracts::CallMethod::new({:?}, {:?}, {:?}, {:?}, {:?}, {lease_literal}, &[]);\n",
                signature.service,
                signature.endpoint,
                signature.endpoint,
                signature.request,
                signature.response,
            ));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_selection(path: &str) -> PreparedSelection {
        PreparedSelection::Path {
            path: path.to_owned(),
        }
    }

    #[test]
    fn prepared_keys_are_injective_across_identity_lookalikes() {
        // Path separators, hyphens, and underscores must never collapse
        // onto one another, and distinct versions must not fold
        // together.
        let a = prepared_key(&path_selection("a/b"), None);
        let b = prepared_key(&path_selection("a-b"), None);
        let c = prepared_key(&path_selection("a_b"), None);
        let keys = [a, b, c];
        for (index, key) in keys.iter().enumerate() {
            for other in keys.iter().skip(index + 1) {
                assert_ne!(key, other, "selection identities must stay distinct");
            }
        }
        let registry = |name: &str, version: &str| {
            prepared_key(
                &PreparedSelection::Registry {
                    registry: "phoxal".to_owned(),
                    name: name.to_owned(),
                    version: version.to_owned(),
                },
                None,
            )
        };
        assert_ne!(registry("pkg", "0.1.0"), registry("pkg", "0-1-0"));
        assert_ne!(
            registry("pkg", "0.1.0"),
            prepared_key(
                &PreparedSelection::Git {
                    name: "pkg".to_owned(),
                    revision: "0.1.0-alpha-prerelease-metadata0000000000000000".to_owned(),
                },
                None
            )
        );
    }

    #[test]
    fn prepared_keys_are_readable_and_distinct_per_binary() {
        let key = prepared_key(
            &PreparedSelection::Registry {
                registry: "phoxal".to_owned(),
                name: "phoxal-service-motion".to_owned(),
                version: "0.0.0-dev.4".to_owned(),
            },
            None,
        );
        // Readable first: the identity is legible in the name, with the
        // complete-identity digest guaranteeing distinctness after it.
        assert!(key.starts_with("registry-phoxal-phoxal-service-motion@0.0.0-dev.4-"));
        assert!(key.ends_with("--bin-default"));
        let alpha = prepared_key(&path_selection("vendor/provider"), Some("alpha"));
        let beta = prepared_key(&path_selection("vendor/provider"), Some("beta"));
        assert!(alpha.ends_with("--bin-alpha"));
        assert_ne!(alpha, beta);
        assert_eq!(
            prepared_key(
                &PreparedSelection::SelfHosted {
                    package: "any".to_owned()
                },
                None
            ),
            "self"
        );
    }

    #[test]
    fn complete_identity_keys_separate_fold_colliding_binaries() {
        // Binary names that fold to the same readable suffix, and an
        // absent versus explicit `default` binary, are distinct
        // selections and must never share a directory.
        let selection = path_selection("provider");
        assert_ne!(
            prepared_key(&selection, Some("sensor-a")),
            prepared_key(&selection, Some("sensor_a"))
        );
        assert_ne!(
            prepared_key(&selection, None),
            prepared_key(&selection, Some("default"))
        );
        assert_ne!(
            prepared_key(&selection, Some("alpha")),
            prepared_key(&selection, Some("beta"))
        );
    }

    #[test]
    fn complete_identity_keys_separate_fold_colliding_sources() {
        // Registry versions and names whose separators fold together,
        // and Git revisions sharing a twelve-character prefix, are
        // distinct identities and must never share a directory.
        let registry = |name: &str, version: &str| {
            prepared_key(
                &PreparedSelection::Registry {
                    registry: "phoxal".to_owned(),
                    name: name.to_owned(),
                    version: version.to_owned(),
                },
                None,
            )
        };
        assert_ne!(registry("pkg", "1.0.0+meta"), registry("pkg", "1.0.0-meta"));
        assert_ne!(registry("a-b", "0.1.0"), registry("a_b", "0.1.0"));
        let git = |revision: &str| {
            prepared_key(
                &PreparedSelection::Git {
                    name: "pkg".to_owned(),
                    revision: revision.to_owned(),
                },
                None,
            )
        };
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let cousin = "0123456789abffffffffffffffffffffffffffffff";
        assert_ne!(git(revision), git(cousin));
    }

    #[test]
    fn read_prepared_for_rejects_a_requested_identity_the_file_does_not_record() {
        // The exact reproduction: a file recording binary `sensor_a`
        // must not be consumed through the directory requested for
        // `sensor-a`, even when the directory name itself matches the
        // recorded (lossy) key.
        let file = PreparedContractFile {
            generation: CONTRACT_GENERATION,
            selection: path_selection("provider"),
            binary: Some("sensor_a".to_owned()),
            executable: PreparedExecutable {
                sha256: "0".repeat(64),
                package: "provider".to_owned(),
                version: None,
            },
            runtime: serde_json::json!({}),
        };
        let dir = Path::new("/robot/.phoxal/prepared").join("tampered-directory");
        // With complete-identity keys these selections no longer share a
        // directory at all; the remaining hazard is a misplaced or
        // tampered file, which the structured comparison on read
        // rejects regardless of directory naming.
        let check =
            |file: &PreparedContractFile, selection: &PreparedSelection, binary: Option<&str>| {
                validate_requested_identity(&dir, file, selection, binary)
            };
        assert!(check(&file, &path_selection("provider"), Some("sensor_a")).is_ok());
        assert!(check(&file, &path_selection("provider"), Some("sensor-a")).is_err());
        assert!(check(&file, &path_selection("provider/other"), Some("sensor_a")).is_err());
        let mut unkeyed = file.clone();
        unkeyed.binary = None;
        assert!(check(&unkeyed, &path_selection("provider"), Some("default")).is_err());
        assert!(check(&unkeyed, &path_selection("provider"), None).is_ok());
    }

    #[test]
    fn prepared_key_validation_rejects_directory_identity_mismatches() {
        let file = PreparedContractFile {
            generation: CONTRACT_GENERATION,
            selection: path_selection("components/ddsm115"),
            binary: None,
            executable: PreparedExecutable {
                sha256: "0".repeat(64),
                package: "phoxal-component-ddsm115".to_owned(),
                version: Some("0.0.0-dev.4".to_owned()),
            },
            runtime: serde_json::json!({}),
        };
        let correct =
            Path::new("/robot/.phoxal/prepared").join(prepared_key(&file.selection, None));
        assert!(validate_prepared_key(&correct, &file).is_ok());
        // A directory named after a DIFFERENT path must be rejected even
        // though it looks structurally valid: the shortened digest is
        // always checked against the recorded complete identity.
        let impostor = Path::new("/robot/.phoxal/prepared")
            .join(prepared_key(&path_selection("components/vl53l1x"), None));
        assert!(validate_prepared_key(&impostor, &file).is_err());
    }

    fn contract_with_outputs(runtime_json: &str) -> PreparedContract {
        PreparedContract {
            file: PreparedContractFile {
                generation: CONTRACT_GENERATION,
                selection: path_selection("participant"),
                binary: None,
                executable: PreparedExecutable {
                    sha256: "0".repeat(64),
                    package: "participant".to_owned(),
                    version: None,
                },
                runtime: serde_json::from_str(runtime_json).expect("runtime record"),
            },
            runtime: serde_json::from_str(runtime_json).expect("runtime record"),
            descriptors: FileDescriptorSet::default(),
        }
    }

    #[test]
    fn endpoints_classify_outputs_by_their_public_shape() {
        // A method-role leased output carries a call signature; its typed
        // helper must bind as a leased call, not an observation.
        let contract = contract_with_outputs(
            r#"{
                "transient_outputs": [
                    {
                        "signature": {
                            "endpoint": "manual",
                            "service": "phoxal.motion.v1.MotionApi",
                            "shape": "call",
                            "request": "phoxal.motion.v1.MotionIntent",
                            "response": "google.protobuf.Empty",
                            "retained_latest": true,
                            "lease_valid_for_ms": 100
                        }
                    }
                ],
                "service_outputs": [
                    {
                        "signature": {
                            "endpoint": "status",
                            "service": "phoxal.motion.v1.MotionApi",
                            "shape": "observation",
                            "request": "google.protobuf.Empty",
                            "response": "phoxal.motion.v1.MotionStatus",
                            "retained_latest": true
                        }
                    }
                ]
            }"#,
        );
        let endpoints = contract.endpoints();
        assert_eq!(endpoints.len(), 2, "both outputs bind as endpoints");
        assert!(matches!(
            &endpoints[0],
            PreparedEndpoint::Call {
                name,
                request,
                response,
                lease_valid_for_ms: Some(100),
            } if name == "manual"
                && request == "phoxal.motion.v1.MotionIntent"
                && response == "google.protobuf.Empty"
        ));
        assert!(matches!(
            &endpoints[1],
            PreparedEndpoint::Observation {
                name,
                response,
                lease_valid_for_ms: None,
            } if name == "status" && response == "phoxal.motion.v1.MotionStatus"
        ));
    }
}
