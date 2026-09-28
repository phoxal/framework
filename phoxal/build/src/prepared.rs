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
use serde::Deserialize;

use crate::Error;

/// Narrow read-model of one retained input record.
#[derive(Clone, Debug, Deserialize)]
struct PreparedInput {
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
#[derive(Clone, Debug, Deserialize)]
struct PreparedOutput {
    #[serde(default)]
    signature: Option<PreparedSignature>,
}

/// Narrow read-model of one retained method signature.
#[derive(Clone, Debug, Deserialize)]
struct PreparedSignature {
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    service: String,
    #[serde(default)]
    shape: String,
    #[serde(default)]
    request: String,
    #[serde(default)]
    response: String,
    #[serde(default)]
    retained_latest: bool,
    #[serde(default)]
    lease_valid_for_ms: Option<u64>,
}

/// Narrow read-model of one retained runtime record.
///
/// The authoritative model is `phoxal::artifact`; this read-model carries
/// only the fields client generation consumes, deserialized from the same
/// retained JSON bytes.
#[derive(Clone, Debug, Deserialize)]
struct PreparedRuntime {
    #[serde(default)]
    inputs: Vec<PreparedInput>,
    #[serde(default)]
    transient_outputs: Vec<PreparedOutput>,
    #[serde(default)]
    service_outputs: Vec<PreparedOutput>,
}

/// The prepared product files of one participant contract.
pub const DESCRIPTORS_FILE: &str = "descriptors.bin";
pub const ENDPOINTS_FILE: &str = "endpoints.json";
/// Records the executable digest the products were extracted from.
pub const PROVENANCE_FILE: &str = "provenance.txt";

/// The marker for a selection whose binary comes from the package default
/// instead of an explicit `binary:` key. Uppercase spelling cannot collide
/// with a valid package or binary name, which are lowercase identifiers.
const DEFAULT_BINARY: &str = "DEFAULT";

/// Encodes one selection path or binary component so distinct selections
/// never share a prepared directory.
///
/// Every byte outside `[A-Za-z0-9]` becomes `_xHH`, so `-` (the component
/// separator in a prepared directory name) never appears inside an encoded
/// component and the encoding is injective: `a/b`, `a-b`, and `a_b` encode
/// to three different names.
#[must_use]
pub fn encode_selection_component(component: &str) -> String {
    let mut encoded = String::with_capacity(component.len());
    for byte in component.bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("_{byte:02x}"));
        }
    }
    encoded
}

/// The prepared-contract directory of one local (path-source) selection.
///
/// `binary` is the selection's explicit `binary:` key, if any; distinct
/// binaries of one package prepare into distinct directories.
#[must_use]
pub fn local_prepared_dir(
    robot_root: &Path,
    relative_source: &Path,
    binary: Option<&str>,
) -> PathBuf {
    let mut identity = String::new();
    for component in relative_source.components() {
        identity.push_str(&encode_selection_component(
            &component.as_os_str().to_string_lossy(),
        ));
        identity.push('-');
    }
    identity.push_str("bin-");
    identity.push_str(&encode_selection_component(
        binary.unwrap_or(DEFAULT_BINARY),
    ));
    robot_root
        .join(".phoxal/local")
        .join(identity)
        .join("contract")
}

/// The prepared-contract directory of one registry or Git selection inside
/// its robot-tree package root.
#[must_use]
pub fn remote_prepared_dir(tree_root: &Path, binary: Option<&str>) -> PathBuf {
    tree_root
        .join(format!(
            "bin-{}",
            encode_selection_component(binary.unwrap_or(DEFAULT_BINARY))
        ))
        .join("contract")
}

/// The prepared-contract directory of a package's own default binary,
/// written by `cargo phoxal prepare` run inside a standalone service
/// package. Its build helper turns these products into local client
/// bindings without compiling the package recursively.
#[must_use]
pub fn self_prepared_dir(package: &Path) -> PathBuf {
    package.join(".phoxal/local/self/contract")
}

/// One participant's prepared contract as the build helper consumes it.
#[derive(Debug, Clone)]
pub struct PreparedContract {
    /// The endpoint and policy metadata retained in the artifact.
    runtime: PreparedRuntime,
    /// The assembled standard descriptor closure.
    pub descriptors: FileDescriptorSet,
}

/// Reads one prepared contract directory.
pub fn read_prepared(contract_dir: &Path) -> Result<PreparedContract, Error> {
    let endpoints = contract_dir.join(ENDPOINTS_FILE);
    let runtime: PreparedRuntime =
        serde_json::from_slice(&fs::read(&endpoints).map_err(|source| Error::Path {
            path: endpoints.clone(),
            source,
        })?)
        .map_err(|error| Error::ApiInput {
            path: contract_dir.to_owned(),
            message: format!("prepared endpoint metadata is invalid: {error}"),
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

    /// Iterates every retained method signature with its input role.
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
fn rust_message_path(
    pool: &prost_reflect::DescriptorPool,
    fqn: &str,
    module_root: &str,
) -> Result<String, Error> {
    if let Some(path) = crate::sdk_type_path(fqn) {
        return Ok(path);
    }
    let message = pool
        .get_message_by_name(fqn)
        .ok_or_else(|| Error::ApiInput {
            path: PathBuf::new(),
            message: format!("prepared closure does not define `{fqn}`"),
        })?;
    let mut segments = vec![module_root.to_owned()];
    segments.extend(package_modules(message.parent_file().package_name())?);
    segments.push(message.name().to_owned());
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
    if ["types", "calls", "projections", "service_methods"].contains(&module.as_str()) {
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

    #[test]
    fn selection_component_encoding_is_injective_across_separator_lookalikes() {
        // Path separators, hyphens, and underscores must never collapse onto
        // one another: the hyphen is the directory-name separator.
        assert_eq!(
            encode_selection_component("a/b"),
            "a_2fb",
            "path separators encode distinctly"
        );
        let hyphen = encode_selection_component("a-b");
        let underscore = encode_selection_component("a_b");
        let dot = encode_selection_component("a.b");
        assert_ne!(hyphen, underscore);
        assert_ne!(hyphen, dot);
        assert_ne!(underscore, dot);
        // A literal that already looks like an escape sequence encodes
        // differently from the input that produced it.
        assert_ne!(
            encode_selection_component("a_2fb"),
            encode_selection_component("a/b")
        );
    }

    #[test]
    fn local_prepared_dirs_distinguish_paths_and_binaries() {
        let root = Path::new("/robot");
        let first = local_prepared_dir(root, Path::new("a/b"), Some("alpha"));
        let second = local_prepared_dir(root, Path::new("a-b"), Some("alpha"));
        let third = local_prepared_dir(root, Path::new("a/b"), Some("beta"));
        let default = local_prepared_dir(root, Path::new("a/b"), None);
        let paths = [
            first.clone(),
            second.clone(),
            third.clone(),
            default.clone(),
        ];
        for (index, path) in paths.iter().enumerate() {
            for other in paths.iter().skip(index + 1) {
                assert_ne!(path, other, "selection identities must stay distinct");
            }
        }
        // A multi-component path joins its encoded components with the
        // separator; a lookalike single component encodes its hyphen.
        assert!(first.to_string_lossy().ends_with("a-b-bin-alpha/contract"));
        assert!(
            second
                .to_string_lossy()
                .ends_with("a_2db-bin-alpha/contract")
        );
        assert!(
            default
                .to_string_lossy()
                .ends_with("a-b-bin-DEFAULT/contract")
        );
    }

    #[test]
    fn remote_prepared_dirs_distinguish_binaries() {
        let root = Path::new("/robot/.phoxal/git/pkg/rev");
        assert_ne!(
            remote_prepared_dir(root, Some("alpha")),
            remote_prepared_dir(root, Some("beta"))
        );
        assert_ne!(
            remote_prepared_dir(root, Some("alpha")),
            remote_prepared_dir(root, None)
        );
    }

    fn contract_with_outputs(runtime_json: &str) -> PreparedContract {
        PreparedContract {
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
