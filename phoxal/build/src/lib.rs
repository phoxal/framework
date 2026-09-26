//! Build-time generation for service-owned Phoxal Protobuf contracts.
//!
//! [`api`] is the package-local generation entry point: one authored service
//! declaration (`service.yaml`, the sections embedded in a component's
//! `component.yaml`, or a robot project's brain section in `robot.yaml`) is
//! the sole endpoint authority, and Protobuf files carry message definitions
//! only. [`compile_protos`] supplies the pinned Protobuf compiler and
//! descriptor retention for message-only compilations such as the SDK's own
//! protocol packages; [`compile_protos_with_output`] names the descriptor
//! file when one build script performs more than one compilation.

use std::path::{Path, PathBuf};
use std::process::Command;

use prost_reflect::DescriptorPool;

mod api;
mod manifest;
mod provider;

pub use api::{BuildApiConfig, api};
pub use api::{
    brain_declaration, participant_declaration, validate_participant_api, validate_project_api,
};
pub use manifest::{
    COMPONENT_FILE_NAME, COMPONENT_SCHEMA, DeclarationEvidence, Delivery, EndpointSide, FILE_NAME,
    ROBOT_SCHEMA, ResolvedService, SCHEMA, ServiceDocument, check_call_binding, check_data_binding,
    check_projection_binding, parse_component_document, parse_document, resolve_document,
};

/// Framework version whose generated API contract this helper implements.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

const DESCRIPTOR_FILE: &str = "phoxal-descriptors.bin";
const MAX_DESCRIPTOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIPTOR_FILES: usize = 1_024;

/// Returns the packaged Protobuf include root containing the built-in
/// robotics vocabulary.
#[must_use]
pub fn include_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("proto")
}

/// Errors reported while compiling or validating a service-owned contract.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A required Cargo build-script environment value is unavailable.
    #[error("Cargo build environment is missing {0}")]
    MissingEnvironment(&'static str),
    /// A source or include path cannot be resolved.
    #[error("cannot resolve Protobuf path {path}: {source}")]
    Path {
        /// The failing path.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
    /// An owned source does not belong to any declared include root.
    #[error("owned Protobuf source {0} is outside every declared include root")]
    SourceOutsideIncludes(PathBuf),
    /// The pinned Protobuf compiler cannot be located.
    #[error("cannot locate the packaged Protobuf compiler: {0}")]
    Protoc(#[from] protoc_bin_vendored::Error),
    /// The Protobuf compiler rejected the contract closure.
    #[error("protoc failed: {0}")]
    ProtocFailed(String),
    /// The retained descriptor closure cannot be read.
    #[error("cannot read generated descriptor set {path}: {source}")]
    ReadDescriptor {
        /// Descriptor-set path.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
    /// The retained descriptors are invalid.
    #[error("invalid Protobuf descriptor closure: {0}")]
    Descriptor(#[from] prost_reflect::DescriptorError),
    /// Two authored schema sources provide different definitions for one
    /// file path or fully-qualified symbol.
    #[error("conflicting Protobuf descriptor {identity} from {first} and {second}")]
    DescriptorConflict {
        /// Conflicting descriptor path or fully-qualified symbol.
        identity: String,
        /// First source declaring the identity.
        first: String,
        /// Second source declaring the identity.
        second: String,
    },
    /// The retained descriptor closure exceeded one of the build-time bounds.
    #[error("Protobuf descriptor closure exceeds {what} bound of {limit} (actual {actual})")]
    DescriptorBounds {
        /// Bounded descriptor quantity.
        what: &'static str,
        /// Maximum accepted value.
        limit: usize,
        /// Observed value.
        actual: usize,
    },
    /// Two generated identifiers collide after normalization.
    #[error("generated {what} name {name} collides in {owner}")]
    NameCollision {
        /// The generated namespace.
        what: &'static str,
        /// Colliding normalized name.
        name: String,
        /// Owning Protobuf package or service.
        owner: String,
    },
    /// Prost generation failed after descriptor validation.
    #[error("cannot generate Rust contract bindings: {0}")]
    Prost(#[from] std::io::Error),
    /// An authored API selection or prepared source tree is invalid.
    #[error("API input {path}: {message}")]
    ApiInput { path: PathBuf, message: String },
    /// A robot document cannot be decoded for build-script generation.
    #[error("cannot parse robot document {path}: {source}")]
    ApiDocument {
        path: PathBuf,
        source: serde_yaml::Error,
    },
}

/// Compiles owned Protobuf message files and their imported closure.
///
/// The explicitly listed files are the owner's files; the retained descriptor
/// closure lands in `OUT_DIR/phoxal-descriptors.bin`. A build script that
/// compiles more than one owner in the same `OUT_DIR` must use
/// [`compile_protos_with_output`] to name a distinct descriptor file per
/// compilation.
pub fn compile_protos(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
) -> Result<(), Error> {
    let out_dir = descriptor_out_dir()?;
    compile_protos_to(protos, includes, &out_dir, &[], DESCRIPTOR_FILE)
}

/// Compiles owned Protobuf message files into a named descriptor output file.
///
/// Equivalent to [`compile_protos`] but writes the retained descriptor closure
/// to `OUT_DIR/<descriptor_file>` instead. A build script that needs multiple
/// descriptor closures in the same `OUT_DIR` — for example one for the
/// framework-owned protocol protos and a separate one for a domain
/// vocabulary — gives each compilation a distinct filename.
pub fn compile_protos_with_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_protos_to(
        protos,
        includes,
        &descriptor_out_dir()?,
        &[],
        descriptor_file,
    )
}

fn descriptor_out_dir() -> Result<PathBuf, Error> {
    std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("OUT_DIR"))
}

fn compile_protos_to(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_protos_impl(
        protos,
        includes,
        out_dir,
        extern_paths,
        descriptor_file,
        None,
        true,
    )
}

fn compile_protos_impl(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
    prost_path: Option<&str>,
    emit_rerun: bool,
) -> Result<(), Error> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let google_include = protoc_bin_vendored::include_path()?;

    let mut include_roots = Vec::with_capacity(includes.len() + 2);
    include_roots.extend(
        includes
            .iter()
            .map(|path| canonical(path.as_ref()))
            .collect::<Result<Vec<_>, _>>()?,
    );
    include_roots.push(canonical(&include_dir())?);
    include_roots.push(canonical(&google_include)?);

    let owned_paths = protos
        .iter()
        .map(|path| canonical(path.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    for path in &owned_paths {
        if !include_roots
            .iter()
            .any(|root| path.strip_prefix(root).is_ok())
        {
            return Err(Error::SourceOutsideIncludes(path.clone()));
        }
    }
    // protoc and prost rewrite their outputs unconditionally; regenerating
    // into a staging area and moving only changed bytes into place keeps
    // unchanged outputs' mtimes stable so dependent crates are not rebuilt.
    let staging = out_dir.join("phoxal-staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|source| Error::Path {
            path: staging.clone(),
            source,
        })?;
    }
    std::fs::create_dir_all(&staging).map_err(|source| Error::Path {
        path: staging.clone(),
        source,
    })?;
    let descriptor_path = staging.join(descriptor_file);
    let mut command = Command::new(&protoc);
    command
        .arg("--include_imports")
        .arg("--include_source_info")
        .arg(format!(
            "--descriptor_set_out={}",
            descriptor_path.display()
        ));
    for include in &include_roots {
        command.arg(format!("--proto_path={}", include.display()));
    }
    command.args(&owned_paths);

    let output = command.output().map_err(|source| Error::Path {
        path: protoc.clone(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::ProtocFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    let descriptor_bytes =
        std::fs::read(&descriptor_path).map_err(|source| Error::ReadDescriptor {
            path: descriptor_path.clone(),
            source,
        })?;
    if descriptor_bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(Error::DescriptorBounds {
            what: "encoded bytes",
            limit: MAX_DESCRIPTOR_BYTES,
            actual: descriptor_bytes.len(),
        });
    }
    let pool = DescriptorPool::decode(descriptor_bytes.as_slice())?;
    let file_count = pool.files().count();
    if file_count > MAX_DESCRIPTOR_FILES {
        return Err(Error::DescriptorBounds {
            what: "file count",
            limit: MAX_DESCRIPTOR_FILES,
            actual: file_count,
        });
    }
    let mut config = prost_build::Config::new();
    config
        .out_dir(&staging)
        .protoc_executable(protoc)
        .file_descriptor_set_path(&descriptor_path)
        .skip_protoc_run()
        .enable_type_names();
    config.compile_well_known_types();
    if let Some(path) = prost_path {
        config.prost_path(path);
    }
    config.extern_path(".google.protobuf.Empty", "::phoxal::contract::Empty");
    for (proto_package, rust_path) in extern_paths {
        config.extern_path(*proto_package, *rust_path);
    }
    config.compile_protos(&owned_paths, &include_roots)?;
    let generated_well_known_types = staging.join("google.protobuf.rs");
    if generated_well_known_types.exists() {
        std::fs::remove_file(generated_well_known_types)?;
    }

    stabilize_generated(&staging, out_dir)?;

    if emit_rerun {
        for path in owned_paths {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize().map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })
}

fn stabilize_generated(staging: &Path, out_dir: &Path) -> Result<(), Error> {
    let entries = std::fs::read_dir(staging).map_err(|source| Error::Path {
        path: staging.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::Path {
            path: staging.to_owned(),
            source,
        })?;
        let source = entry.path();
        if !source.is_file() {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        let destination = out_dir.join(name);
        if destination.is_file()
            && std::fs::read(&destination).is_ok_and(|existing| {
                std::fs::read(&source).is_ok_and(|generated| existing == generated)
            })
        {
            std::fs::remove_file(&source).map_err(|source| Error::Path {
                path: staging.to_owned(),
                source,
            })?;
            continue;
        }
        std::fs::rename(&source, &destination).map_err(|source| Error::Path {
            path: destination.clone(),
            source,
        })?;
    }
    std::fs::remove_dir(staging).map_err(|source| Error::Path {
        path: staging.to_owned(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use prost::Message as _;
    use prost_reflect::{DescriptorPool, DynamicMessage, Value};

    use super::{DESCRIPTOR_FILE, Error, compile_protos_to, include_dir};

    fn compile_sources(
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, tempfile::TempDir, Result<(), Error>) {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let mut paths = Vec::with_capacity(files.len());
        for (relative, contents) in files {
            let path = source.path().join(relative);
            fs::create_dir_all(path.parent().expect("fixture has a parent directory"))
                .expect("fixture directory");
            fs::write(&path, contents).expect("fixture source");
            paths.push(path);
        }
        let result = compile_protos_to(
            &paths,
            &[source.path()],
            output.path(),
            &[],
            DESCRIPTOR_FILE,
        );
        (source, output, result)
    }

    const SHARED_PAYLOAD: &str = r#"
        syntax = "proto3";
        package example.shared.v1;
        message SharedPayload { uint64 value = 1; }
    "#;

    #[test]
    fn packaged_robotics_schema_is_available_to_generation() {
        assert!(
            include_dir()
                .join("phoxal/robotics/v1/robotics.proto")
                .is_file()
        );
    }

    #[test]
    fn compiles_message_only_sources_without_endpoint_surface() {
        let (_source, output, result) = compile_sources(&[(
            "example/vocabulary/v1/vocabulary.proto",
            r#"
                syntax = "proto3";
                package example.vocabulary.v1;
                message Measurement { optional double value = 1; }
            "#,
        )]);
        result.expect("message-only generation");

        let generated = fs::read_to_string(output.path().join("example.vocabulary.v1.rs"))
            .expect("generated Rust");
        assert!(generated.contains("pub struct Measurement"));
        // Protobuf files carry message definitions only; no endpoint surface
        // is ever derived from them.
        assert!(!generated.contains("CallMethod"));
        assert!(!generated.contains("ObservationMethod"));
        assert!(!generated.contains("descriptor_frame"));
        assert!(output.path().join(DESCRIPTOR_FILE).is_file());
    }

    #[test]
    fn exchanges_an_owned_message_with_an_independent_python_peer() {
        let (_source, output, result) =
            compile_sources(&[("example/shared/v1/payload.proto", SHARED_PAYLOAD)]);
        result.expect("message generation");
        let descriptors = fs::read(output.path().join(DESCRIPTOR_FILE)).expect("descriptors");
        let pool = DescriptorPool::decode(descriptors.as_slice()).expect("descriptor closure");
        let descriptor = pool
            .get_message_by_name("example.shared.v1.SharedPayload")
            .expect("owned message descriptor");
        let mut request = DynamicMessage::new(descriptor.clone());
        request
            .try_set_field_by_name("value", Value::U64(150))
            .expect("fixture value");

        let script = r#"
import sys

payload = sys.stdin.buffer.read()
if payload != bytes((0x08, 0x96, 0x01)):
    raise SystemExit(f"unexpected protobuf payload: {payload.hex()}")
sys.stdout.buffer.write(bytes((0x08, 0xAC, 0x02)))
"#;
        let mut peer = Command::new("python3")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("independent Python peer starts");
        peer.stdin
            .as_mut()
            .expect("Python stdin")
            .write_all(&request.encode_to_vec())
            .expect("Rust request reaches Python");
        drop(peer.stdin.take());
        let reply = peer.wait_with_output().expect("Python peer completes");
        assert!(reply.status.success(), "Python peer rejected the request");

        let reply = DynamicMessage::decode(descriptor, reply.stdout.as_slice())
            .expect("Python reply decodes in Rust");
        assert_eq!(
            reply
                .get_field_by_name("value")
                .expect("reply value")
                .as_ref(),
            &Value::U64(300)
        );
    }

    #[test]
    fn rejects_owned_sources_outside_include_roots() {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let outside = tempfile::NamedTempFile::new().expect("temporary outside source");
        let result = compile_protos_to(
            &[PathBuf::from(outside.path())],
            &[source.path()],
            output.path(),
            &[],
            DESCRIPTOR_FILE,
        );
        assert!(matches!(result, Err(Error::SourceOutsideIncludes(_))));
    }
}
