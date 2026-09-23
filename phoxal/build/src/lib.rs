//! Build-time generation for service-owned Phoxal Protobuf contracts.
//!
//! [`compile_protos`] supplies a pinned Protobuf compiler, emits Prost messages
//! with type names, and retains the original descriptor closure.
//! [`compile_contracts`] additionally generates inert typed call and
//! observation descriptors from ordinary Protobuf method cardinality.
//!
//! Two flags tune what a build script needs:
//!
//! * **dependency descriptors** — [`compile_protos_with_dependencies`] (and
//!   [`compile_protos_with_dependencies_and_output`]) additionally import
//!   descriptor sets from build dependencies and map their packages to
//!   canonical Rust contract crates, so a service can consume one shared wire
//!   vocabulary without regenerating or locating dependency-owned source
//!   files.
//! * **descriptor output filename** — [`compile_protos_with_output`] (and
//!   [`compile_protos_with_dependencies_and_output`]) name the file the
//!   retained descriptor closure lands in. A build script that performs more
//!   than one compilation in the same `OUT_DIR` must give each invocation a
//!   distinct filename; the two simple forms write `DESCRIPTOR_FILE` and
//!   are the right call for a single compilation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost_reflect::{DescriptorPool, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod api;

#[doc(hidden)]
pub use api::validate_participant_api;
pub use api::{BuildApiConfig, api};

/// The replacement call/observation option definition packaged with this crate.
pub const API_PROTO: &str = include_str!("../proto/phoxal/api.proto");

const DESCRIPTOR_FILE: &str = "phoxal-descriptors.bin";
/// Generated freshness and dependency-closure evidence.
pub const CONTRACT_METADATA_FILE: &str = "phoxal-contract.json";
const DEPENDENCY_DESCRIPTOR_FILE: &str = "phoxal-dependency-descriptors.bin";
const RETAINED_LATEST_EXTENSION: &str = "phoxal.api.retained_latest";
const LEASE_EXTENSION: &str = "phoxal.api.lease";
const MAX_DESCRIPTOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIPTOR_FILES: usize = 1_024;

/// Returns the packaged Protobuf include root containing `phoxal/api.proto`.
#[must_use]
pub fn include_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("proto")
}

/// Returns the descriptor-set path emitted into the current Cargo `OUT_DIR`.
///
/// Contract libraries can embed these exact bytes with `include_bytes!`.
pub fn descriptor_set_path() -> Result<PathBuf, Error> {
    std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .map(|out_dir| out_dir.join(DESCRIPTOR_FILE))
        .ok_or(Error::MissingEnvironment("OUT_DIR"))
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
    /// Two descriptor dependencies provide different definitions for one file
    /// path or fully-qualified symbol.
    #[error("conflicting Protobuf descriptor {identity} from dependencies {first} and {second}")]
    DescriptorConflict {
        /// Conflicting descriptor path or fully-qualified symbol.
        identity: String,
        /// First dependency declaring the identity.
        first: String,
        /// Second dependency declaring the identity.
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
    /// A public contract method uses unsupported Protobuf cardinality.
    #[error("owned method {method} has unsupported shape: {reason}")]
    UnsupportedMethodShape {
        /// Fully-qualified Protobuf method.
        method: String,
        /// Why the method cannot become a call or observation.
        reason: &'static str,
    },
    /// A contract modifier is incompatible with the method shape or value.
    #[error("owned method {method} has invalid {modifier}: {reason}")]
    InvalidModifier {
        /// Fully-qualified Protobuf method.
        method: String,
        /// Option name.
        modifier: &'static str,
        /// Validation failure.
        reason: &'static str,
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
    /// Generated contract metadata could not be encoded.
    #[error("cannot encode generated contract metadata: {0}")]
    Metadata(#[from] serde_json::Error),
    /// Checked-in contract evidence does not match its authored or generated files.
    #[error("contract metadata mismatch at {path}: {message}")]
    ContractMetadataMismatch { path: PathBuf, message: String },
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

/// Compiles owned Protobuf files and their imported descriptor closure.
///
/// The explicitly listed files are the contract owner's files.
/// Imported services remain dependency descriptors and do not generate Phoxal
/// ports in this build. The retained descriptor closure lands in
/// `OUT_DIR/<DESCRIPTOR_FILE>`; a build script that compiles more than one
/// owner in the same `OUT_DIR` must use [`compile_protos_with_output`] (or
/// [`compile_protos_with_dependencies_and_output`]) to name a distinct
/// descriptor file per compilation.
pub fn compile_protos(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
) -> Result<(), Error> {
    compile_protos_with_dependencies(protos, includes, &[], &[])
}

/// Compiles owned Protobuf files into a named descriptor output file.
///
/// Equivalent to [`compile_protos`] but writes the retained descriptor closure
/// to `OUT_DIR/<descriptor_file>` instead of `DESCRIPTOR_FILE`. A build
/// script that needs multiple descriptor closures in the same `OUT_DIR` —
/// for example one for the framework-owned protocol protos and a separate one
/// for a domain vocabulary — gives each compilation a distinct filename.
pub fn compile_protos_with_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_to_with_dependencies(
        protos,
        includes,
        &descriptor_out_dir()?,
        &[],
        &[],
        descriptor_file,
    )
}

/// One descriptor closure supplied by a direct Cargo build dependency.
#[derive(Clone, Copy, Debug)]
pub struct DependencyDescriptor<'a> {
    /// Human-readable Cargo package name used in conflict diagnostics.
    pub package: &'a str,
    /// The dependency owner's original encoded `FileDescriptorSet`.
    pub descriptors: &'a [u8],
}

impl<'a> DependencyDescriptor<'a> {
    /// Creates one exact dependency descriptor input.
    #[must_use]
    pub const fn new(package: &'a str, descriptors: &'a [u8]) -> Self {
        Self {
            package,
            descriptors,
        }
    }
}

/// Compiles owned Protobuf files using descriptor closures exported by direct
/// build dependencies.
///
/// Each tuple contains the fully-qualified Protobuf package (including its
/// leading dot) and the Rust path Prost should use for that package. The
/// imported descriptors remain in the owner's descriptor closure, while their
/// messages are referenced rather than regenerated. Dependency source trees
/// are deliberately not accepted as include roots. The retained descriptor
/// closure lands in `OUT_DIR/<DESCRIPTOR_FILE>`; the descriptor-filename
/// variant [`compile_protos_with_dependencies_and_output`] is the right call
/// when a build script needs both dependency imports and a non-default
/// descriptor filename.
pub fn compile_protos_with_dependencies(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
) -> Result<(), Error> {
    compile_protos_with_dependencies_and_output(
        protos,
        includes,
        dependencies,
        extern_paths,
        DESCRIPTOR_FILE,
    )
}

/// Compiles owned Protobuf files using dependency descriptors and writes the
/// retained closure to a named descriptor output file.
///
/// Combines [`compile_protos_with_dependencies`] with [`compile_protos_with_output`]:
/// a build script that imports descriptor closures from build dependencies
/// *and* must keep more than one closure distinct in the same `OUT_DIR` —
/// for example the framework compilation next to a domain-vocabulary
/// compilation — gives each invocation a distinct filename.
pub fn compile_protos_with_dependencies_and_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    let out_dir = descriptor_out_dir()?;
    compile_to_with_dependencies(
        protos,
        includes,
        &out_dir,
        dependencies,
        extern_paths,
        descriptor_file,
    )
}

/// Compiles one owner contract using cardinality-derived calls and observations.
///
/// This is the replacement service-package generator. It accepts only unary
/// calls and empty-request server-stream observations, and emits generated
/// method descriptors backed by [`phoxal::contract`](https://docs.rs/phoxal).
pub fn compile_contracts(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
) -> Result<(), Error> {
    compile_contracts_with_dependencies(protos, includes, &[], &[])
}

/// Compiles a replacement contract and names its retained descriptor output.
pub fn compile_contracts_with_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_contracts_to_with_dependencies(
        protos,
        includes,
        &descriptor_out_dir()?,
        &[],
        &[],
        descriptor_file,
    )
}

/// Compiles a replacement contract using dependency-owned descriptor closures.
pub fn compile_contracts_with_dependencies(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
) -> Result<(), Error> {
    compile_contracts_with_dependencies_and_output(
        protos,
        includes,
        dependencies,
        extern_paths,
        DESCRIPTOR_FILE,
    )
}

/// Compiles a replacement contract with dependency closures and named output.
pub fn compile_contracts_with_dependencies_and_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_contracts_to_with_dependencies(
        protos,
        includes,
        &descriptor_out_dir()?,
        dependencies,
        extern_paths,
        descriptor_file,
    )
}

/// Generates checked-in contract artifacts into an explicit candidate directory.
///
/// Project tooling calls this against a temporary directory and installs the
/// complete candidate atomically. Build scripts should use [`compile_contracts`]
/// so Cargo owns their `OUT_DIR` lifecycle.
pub fn generate_contract_package(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
) -> Result<(), Error> {
    std::fs::create_dir_all(out_dir).map_err(|source| Error::Path {
        path: out_dir.to_owned(),
        source,
    })?;
    compile_contracts_to_with_dependencies(
        protos,
        includes,
        out_dir,
        dependencies,
        extern_paths,
        DESCRIPTOR_FILE,
    )?;
    let mut generated = std::fs::read_dir(out_dir)
        .map_err(|source| Error::Path {
            path: out_dir.to_owned(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "rs")
                && path.file_name().is_some_and(|name| name != "lib.rs")
        })
        .collect::<Vec<_>>();
    generated.sort();
    let mut library = String::from("// @generated by phoxal-build; do not edit.\n");
    for path in generated {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(Error::Path {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "generated Rust filename is not UTF-8",
                ),
            });
        };
        library.push_str("include!(");
        library.push_str(&format!("{name:?}"));
        library.push_str(");\n");
    }
    for (proto_package, rust_path) in extern_paths {
        let module = contract_import_module(proto_package);
        library.push_str("#[doc(hidden)]\npub mod ");
        library.push_str(&module);
        library.push_str(" {\n    pub use ");
        library.push_str(rust_path);
        library.push_str("::*;\n}\n");
    }
    library.push_str(
        "/// Exact generated descriptor closure.\n\
         pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(\"phoxal-descriptors.bin\");\n",
    );
    std::fs::write(out_dir.join("lib.rs"), library).map_err(|source| Error::Path {
        path: out_dir.join("lib.rs"),
        source,
    })?;
    write_contract_metadata(protos, includes, out_dir, dependencies)?;
    Ok(())
}

fn contract_import_module(proto_package: &str) -> String {
    let mut module = String::from("__phoxal_contract_import_");
    for character in proto_package.trim_start_matches('.').chars() {
        if character.is_ascii_alphanumeric() {
            module.push(character.to_ascii_lowercase());
        } else {
            module.push('_');
        }
    }
    module
}

#[derive(Deserialize, Serialize)]
struct ContractMetadata {
    format: u32,
    generator: String,
    descriptor_sha256: String,
    sources: Vec<ContractFile>,
    generated: Vec<ContractFile>,
    dependencies: Vec<ContractDependency>,
}

#[derive(Deserialize, Serialize)]
struct ContractFile {
    path: String,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
struct ContractDependency {
    package: String,
    descriptor_sha256: String,
}

fn write_contract_metadata(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
) -> Result<(), Error> {
    let include_roots = includes
        .iter()
        .map(|path| canonical(path.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut sources = protos
        .iter()
        .map(|path| {
            let path = canonical(path.as_ref())?;
            let relative = include_roots
                .iter()
                .find_map(|include| path.strip_prefix(include).ok())
                .ok_or_else(|| Error::SourceOutsideIncludes(path.clone()))?;
            Ok(ContractFile {
                path: proto_name(relative),
                sha256: file_sha256(&path)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    sources.sort_by(|left, right| left.path.cmp(&right.path));

    let descriptor = out_dir.join(DESCRIPTOR_FILE);
    let descriptor_sha256 = file_sha256(&descriptor)?;
    let mut generated = std::fs::read_dir(out_dir)
        .map_err(|source| Error::Path {
            path: out_dir.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| Error::Path {
            path: out_dir.to_owned(),
            source,
        })?
        .into_iter()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| entry.file_name() != CONTRACT_METADATA_FILE)
        .map(|entry| {
            let path = entry.path();
            Ok(ContractFile {
                path: entry.file_name().to_string_lossy().into_owned(),
                sha256: file_sha256(&path)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    generated.sort_by(|left, right| left.path.cmp(&right.path));

    let mut dependency_metadata = dependencies
        .iter()
        .map(|dependency| ContractDependency {
            package: dependency.package.to_owned(),
            descriptor_sha256: bytes_sha256(dependency.descriptors),
        })
        .collect::<Vec<_>>();
    dependency_metadata.sort_by(|left, right| left.package.cmp(&right.package));

    let metadata = ContractMetadata {
        format: 1,
        generator: env!("CARGO_PKG_VERSION").to_owned(),
        descriptor_sha256,
        sources,
        generated,
        dependencies: dependency_metadata,
    };
    let mut encoded = serde_json::to_vec_pretty(&metadata)?;
    encoded.push(b'\n');
    std::fs::write(out_dir.join(CONTRACT_METADATA_FILE), encoded).map_err(|source| {
        Error::Path {
            path: out_dir.join(CONTRACT_METADATA_FILE),
            source,
        }
    })?;
    Ok(())
}

/// Verifies checked-in contract files without rewriting the package.
pub fn verify_contract_metadata(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    generated_dir: &Path,
) -> Result<(), Error> {
    let metadata_path = generated_dir.join(CONTRACT_METADATA_FILE);
    let encoded = std::fs::read(&metadata_path).map_err(|source| Error::Path {
        path: metadata_path.clone(),
        source,
    })?;
    let metadata: ContractMetadata = serde_json::from_slice(&encoded)?;
    if metadata.format != 1 {
        return Err(Error::ContractMetadataMismatch {
            path: metadata_path,
            message: format!("unsupported format {}", metadata.format),
        });
    }
    if metadata.generator != env!("CARGO_PKG_VERSION") {
        return Err(Error::ContractMetadataMismatch {
            path: generated_dir.join(CONTRACT_METADATA_FILE),
            message: format!(
                "generator {} is incompatible with {}",
                metadata.generator,
                env!("CARGO_PKG_VERSION")
            ),
        });
    }

    let include_roots = includes
        .iter()
        .map(|path| canonical(path.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut actual_sources = protos
        .iter()
        .map(|path| {
            let path = canonical(path.as_ref())?;
            let relative = include_roots
                .iter()
                .find_map(|include| path.strip_prefix(include).ok())
                .ok_or_else(|| Error::SourceOutsideIncludes(path.clone()))?;
            Ok(ContractFile {
                path: proto_name(relative),
                sha256: file_sha256(&path)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    actual_sources.sort_by(|left, right| left.path.cmp(&right.path));
    if !same_contract_files(&metadata.sources, &actual_sources) {
        return Err(Error::ContractMetadataMismatch {
            path: generated_dir.join(CONTRACT_METADATA_FILE),
            message: "authored Protobuf sources differ from accepted generation".to_owned(),
        });
    }

    let mut actual_generated = std::fs::read_dir(generated_dir)
        .map_err(|source| Error::Path {
            path: generated_dir.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| Error::Path {
            path: generated_dir.to_owned(),
            source,
        })?
        .into_iter()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| entry.file_name() != CONTRACT_METADATA_FILE)
        .map(|entry| {
            let path = entry.path();
            Ok(ContractFile {
                path: entry.file_name().to_string_lossy().into_owned(),
                sha256: file_sha256(&path)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    actual_generated.sort_by(|left, right| left.path.cmp(&right.path));
    if !same_contract_files(&metadata.generated, &actual_generated) {
        return Err(Error::ContractMetadataMismatch {
            path: generated_dir.join(CONTRACT_METADATA_FILE),
            message: "generated contract files differ from accepted generation".to_owned(),
        });
    }
    if metadata.descriptor_sha256 != file_sha256(&generated_dir.join(DESCRIPTOR_FILE))? {
        return Err(Error::ContractMetadataMismatch {
            path: generated_dir.join(DESCRIPTOR_FILE),
            message: "descriptor digest differs from accepted generation".to_owned(),
        });
    }
    Ok(())
}

/// Verifies the exact direct descriptor inputs recorded during generation.
pub fn verify_contract_dependencies(
    generated_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
) -> Result<(), Error> {
    let metadata_path = generated_dir.join(CONTRACT_METADATA_FILE);
    let encoded = std::fs::read(&metadata_path).map_err(|source| Error::Path {
        path: metadata_path.clone(),
        source,
    })?;
    let metadata: ContractMetadata = serde_json::from_slice(&encoded)?;
    let mut actual = dependencies
        .iter()
        .map(|dependency| ContractDependency {
            package: dependency.package.to_owned(),
            descriptor_sha256: bytes_sha256(dependency.descriptors),
        })
        .collect::<Vec<_>>();
    actual.sort_by(|left, right| left.package.cmp(&right.package));
    if metadata.dependencies.len() != actual.len()
        || metadata
            .dependencies
            .iter()
            .zip(actual)
            .any(|(left, right)| {
                left.package != right.package || left.descriptor_sha256 != right.descriptor_sha256
            })
    {
        return Err(Error::ContractMetadataMismatch {
            path: metadata_path,
            message: "resolved dependency descriptors differ from accepted generation".to_owned(),
        });
    }
    Ok(())
}

fn same_contract_files(left: &[ContractFile], right: &[ContractFile]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.path == right.path && left.sha256 == right.sha256)
}

fn file_sha256(path: &Path) -> Result<String, Error> {
    let bytes = std::fs::read(path).map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })?;
    Ok(bytes_sha256(&bytes))
}

fn bytes_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn descriptor_out_dir() -> Result<PathBuf, Error> {
    std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("OUT_DIR"))
}

#[cfg(test)]
fn compile_to(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_to_with_dependencies(protos, includes, out_dir, &[], &[], descriptor_file)
}

fn compile_to_with_dependencies(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_contracts_to_with_dependencies(
        protos,
        includes,
        out_dir,
        dependencies,
        extern_paths,
        descriptor_file,
    )
}

fn compile_contracts_to_with_dependencies(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_contracts_impl(
        protos,
        includes,
        out_dir,
        dependencies,
        extern_paths,
        descriptor_file,
        None,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_contracts_impl(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
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
    let owned_names = owned_file_names(&owned_paths, &include_roots)?;
    let descriptor_path = out_dir.join(descriptor_file);
    let dependency_descriptor_path = if dependencies.is_empty() {
        None
    } else {
        let path = out_dir.join(DEPENDENCY_DESCRIPTOR_FILE);
        let bytes = merge_dependency_descriptors(dependencies)?;
        std::fs::write(&path, bytes).map_err(|source| Error::Path {
            path: path.clone(),
            source,
        })?;
        Some(path)
    };

    let mut command = Command::new(&protoc);
    command
        .arg("--include_imports")
        .arg("--include_source_info")
        .arg(format!(
            "--descriptor_set_out={}",
            descriptor_path.display()
        ));
    if let Some(path) = &dependency_descriptor_path {
        command.arg(format!("--descriptor_set_in={}", path.display()));
    }
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
    if let Some(path) = &dependency_descriptor_path {
        std::fs::remove_file(path).map_err(|source| Error::Path {
            path: path.clone(),
            source,
        })?;
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
    let methods = validate_owned_methods(&pool, &owned_names)?;
    let service_package = owner_service_package(methods.keys());
    let service_generator: Box<dyn prost_build::ServiceGenerator> =
        Box::new(ContractGenerator { methods });
    let has_services = service_package.is_some();
    let mut config = prost_build::Config::new();
    config
        .out_dir(out_dir)
        .protoc_executable(protoc)
        .file_descriptor_set_path(&descriptor_path)
        .skip_protoc_run()
        .enable_type_names()
        .service_generator(service_generator);
    config.compile_well_known_types();
    if let Some(path) = prost_path {
        config.prost_path(path);
    }
    config.extern_path(".google.protobuf.Empty", "::phoxal::contract::Empty");
    for (proto_package, rust_path) in extern_paths {
        config.extern_path(*proto_package, *rust_path);
    }
    config.compile_protos(&owned_paths, &include_roots)?;
    let generated_well_known_types = out_dir.join("google.protobuf.rs");
    if generated_well_known_types.exists() {
        std::fs::remove_file(generated_well_known_types)?;
    }

    if has_services {
        embed_descriptor_section(
            out_dir,
            service_package.as_deref().unwrap_or_default(),
            descriptor_bytes.len(),
            descriptor_file,
        )?;
    }

    if emit_rerun {
        for path in owned_paths {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        println!(
            "cargo:rerun-if-changed={}",
            include_dir().join("phoxal/api.proto").display()
        );
    }
    Ok(())
}

fn owner_service_package<'a>(methods: impl Iterator<Item = &'a String>) -> Option<String> {
    methods.into_iter().next().and_then(|name| {
        name.rsplit_once('.')
            .and_then(|(prefix, _)| prefix.rsplit_once('.'))
            .map(|(package, _)| package.to_owned())
    })
}

fn merge_dependency_descriptors(
    dependencies: &[DependencyDescriptor<'_>],
) -> Result<Vec<u8>, Error> {
    let mut known_files = HashMap::<String, (String, Vec<u8>)>::new();
    let mut known_symbols = HashMap::<String, (String, String)>::new();
    let mut pools = Vec::with_capacity(dependencies.len());

    for dependency in dependencies {
        if dependency.descriptors.len() > MAX_DESCRIPTOR_BYTES {
            return Err(Error::DescriptorBounds {
                what: "dependency encoded bytes",
                limit: MAX_DESCRIPTOR_BYTES,
                actual: dependency.descriptors.len(),
            });
        }
        let pool = DescriptorPool::decode(dependency.descriptors)?;
        let file_count = pool.files().count();
        if file_count > MAX_DESCRIPTOR_FILES {
            return Err(Error::DescriptorBounds {
                what: "dependency file count",
                limit: MAX_DESCRIPTOR_FILES,
                actual: file_count,
            });
        }

        for file in pool.files() {
            let name = file.name().to_owned();
            let encoded = file.encode_to_vec();
            if let Some((first, first_encoded)) = known_files.get(&name) {
                if first_encoded != &encoded {
                    return Err(Error::DescriptorConflict {
                        identity: name,
                        first: first.clone(),
                        second: dependency.package.to_owned(),
                    });
                }
                continue;
            }

            for identity in descriptor_symbols(&pool, &name) {
                if let Some((first, first_file)) = known_symbols.get(&identity)
                    && first_file != &name
                {
                    return Err(Error::DescriptorConflict {
                        identity,
                        first: first.clone(),
                        second: dependency.package.to_owned(),
                    });
                }
                known_symbols.insert(identity, (dependency.package.to_owned(), name.clone()));
            }
            known_files.insert(name, (dependency.package.to_owned(), encoded));
        }
        pools.push(pool);
    }

    let mut merged = DescriptorPool::new();
    for (dependency, pool) in dependencies.iter().zip(pools) {
        merged
            .decode_file_descriptor_set(pool.encode_to_vec().as_slice())
            .map_err(|_| Error::DescriptorConflict {
                identity: "descriptor closure".to_owned(),
                first: "previous dependencies".to_owned(),
                second: dependency.package.to_owned(),
            })?;
    }
    Ok(merged.encode_to_vec())
}

fn descriptor_symbols(pool: &DescriptorPool, file_name: &str) -> Vec<String> {
    let messages = pool
        .all_messages()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    let enums = pool
        .all_enums()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    let services = pool
        .services()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    let extensions = pool
        .all_extensions()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    messages
        .chain(enums)
        .chain(services)
        .chain(extensions)
        .collect()
}

fn canonical(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize().map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })
}

fn owned_file_names(protos: &[PathBuf], includes: &[PathBuf]) -> Result<HashSet<String>, Error> {
    protos
        .iter()
        .map(|proto| {
            includes
                .iter()
                .find_map(|include| proto.strip_prefix(include).ok())
                .map(proto_name)
                .ok_or_else(|| Error::SourceOutsideIncludes(proto.clone()))
        })
        .collect()
}

fn proto_name(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn embed_descriptor_section(
    out_dir: &Path,
    service_package: &str,
    descriptor_len: usize,
    descriptor_file: &str,
) -> Result<(), Error> {
    let generated = out_dir.join(format!("{service_package}.rs"));
    let frame_len = descriptor_len
        .checked_add(phoxal_port_frame_header_bytes())
        .ok_or(Error::DescriptorBounds {
            what: "framed bytes",
            limit: usize::MAX,
            actual: descriptor_len,
        })?;
    let section = format!(
        "\n#[doc(hidden)]\n#[used]\n#[cfg_attr(target_os = \"macos\", unsafe(link_section = \"__DATA,__phoxal_desc\"))]\n#[cfg_attr(not(target_os = \"macos\"), unsafe(link_section = \".phoxal_desc\"))]\nstatic __PHOXAL_DESCRIPTOR_SET: [u8; {frame_len}] = ::phoxal::contract::descriptor_frame::<{frame_len}>(include_bytes!({descriptor_file:?}));\n"
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&generated)
        .map_err(|source| Error::Prost(std::io::Error::new(source.kind(), source)))?;
    use std::io::Write;
    file.write_all(section.as_bytes())?;
    Ok(())
}

const fn phoxal_port_frame_header_bytes() -> usize {
    16
}

#[derive(Clone, Copy, Debug)]
enum ContractShape {
    Call,
    Observation,
}

#[derive(Clone, Debug)]
struct MethodSpec {
    shape: ContractShape,
    public_name: String,
    constant_name: String,
    retained_latest: bool,
    lease_valid_for_ms: Option<u64>,
}

fn validate_owned_methods(
    pool: &DescriptorPool,
    owned_names: &HashSet<String>,
) -> Result<HashMap<String, MethodSpec>, Error> {
    let retained_latest = pool.get_extension_by_name(RETAINED_LATEST_EXTENSION);
    let lease = pool.get_extension_by_name(LEASE_EXTENSION);
    let mut methods = HashMap::new();
    let mut package_modules: HashMap<String, HashSet<String>> = HashMap::new();

    for file in pool
        .files()
        .filter(|file| owned_names.contains(file.name()))
    {
        for service in file.services() {
            let module_name = service.name().to_snake_case();
            if !package_modules
                .entry(service.package_name().to_owned())
                .or_default()
                .insert(module_name.clone())
            {
                return Err(Error::NameCollision {
                    what: "service module",
                    name: module_name,
                    owner: service.package_name().to_owned(),
                });
            }

            let mut public_names = HashSet::new();
            let mut constants = HashSet::new();
            for method in service.methods() {
                let full_name = method.full_name().to_owned();
                let shape = contract_shape(&method)?;
                let options = method.options();
                let retained = retained_latest.as_ref().is_some_and(|extension| {
                    matches!(options.get_extension(extension).as_ref(), Value::Bool(true))
                });
                if retained && !matches!(shape, ContractShape::Observation) {
                    return Err(Error::InvalidModifier {
                        method: full_name,
                        modifier: RETAINED_LATEST_EXTENSION,
                        reason: "it is valid only for observations",
                    });
                }
                let lease_valid_for_ms = lease
                    .as_ref()
                    .filter(|extension| options.has_extension(extension))
                    .map(|extension| {
                        let option_value = options.get_extension(extension);
                        let Value::Message(value) = option_value.as_ref() else {
                            return Err(Error::InvalidModifier {
                                method: full_name.clone(),
                                modifier: LEASE_EXTENSION,
                                reason: "the option is not a Lease message",
                            });
                        };
                        let Some(value) = value.get_field_by_name("valid_for_ms") else {
                            return Err(Error::InvalidModifier {
                                method: full_name.clone(),
                                modifier: LEASE_EXTENSION,
                                reason: "valid_for_ms is missing",
                            });
                        };
                        let Value::U64(value) = value.as_ref() else {
                            return Err(Error::InvalidModifier {
                                method: full_name.clone(),
                                modifier: LEASE_EXTENSION,
                                reason: "valid_for_ms is not an unsigned integer",
                            });
                        };
                        if *value == 0 {
                            return Err(Error::InvalidModifier {
                                method: full_name.clone(),
                                modifier: LEASE_EXTENSION,
                                reason: "valid_for_ms must be positive",
                            });
                        }
                        Ok(*value)
                    })
                    .transpose()?;

                let public_name = method.name().to_snake_case();
                let constant_name = method.name().to_shouty_snake_case();
                if !public_names.insert(public_name.clone()) {
                    return Err(Error::NameCollision {
                        what: "public method",
                        name: public_name,
                        owner: service.full_name().to_owned(),
                    });
                }
                if !constants.insert(constant_name.clone()) {
                    return Err(Error::NameCollision {
                        what: "Rust constant",
                        name: constant_name,
                        owner: service.full_name().to_owned(),
                    });
                }
                methods.insert(
                    full_name,
                    MethodSpec {
                        shape,
                        public_name,
                        constant_name,
                        retained_latest: retained,
                        lease_valid_for_ms,
                    },
                );
            }
        }
    }
    Ok(methods)
}

fn contract_shape(method: &prost_reflect::MethodDescriptor) -> Result<ContractShape, Error> {
    let method_name = method.full_name().to_owned();
    if method.is_client_streaming() {
        return Err(Error::UnsupportedMethodShape {
            method: method_name,
            reason: "client streaming is not supported",
        });
    }
    if method.is_server_streaming() {
        if method.input().full_name() != "google.protobuf.Empty" {
            return Err(Error::UnsupportedMethodShape {
                method: method_name,
                reason: "an observation must accept google.protobuf.Empty",
            });
        }
        Ok(ContractShape::Observation)
    } else {
        Ok(ContractShape::Call)
    }
}

struct ContractGenerator {
    methods: HashMap<String, MethodSpec>,
}

impl prost_build::ServiceGenerator for ContractGenerator {
    fn generate(&mut self, service: prost_build::Service, buffer: &mut String) {
        if !service.methods.iter().any(|method| {
            let full_name = format!(
                "{}.{}.{}",
                service.package, service.proto_name, method.proto_name
            );
            self.methods.contains_key(&full_name)
        }) {
            let module_name = service.proto_name.to_snake_case();
            buffer.push_str(&format!("pub mod {module_name} {{}}\n"));
            return;
        }

        let module_name = service.proto_name.to_snake_case();
        let service_name = if service.package.is_empty() {
            service.proto_name.clone()
        } else {
            format!("{}.{}", service.package, service.proto_name)
        };
        buffer.push_str(&format!("pub mod {module_name} {{\n"));
        buffer.push_str("    pub mod methods {\n");
        for method in &service.methods {
            let full_name = format!(
                "{}.{}.{}",
                service.package, service.proto_name, method.proto_name
            );
            let spec = &self.methods[&full_name];
            let input_type = nested_contract_type(&method.input_type);
            let output_type = nested_contract_type(&method.output_type);
            let request_name = proto_type_name(&method.input_proto_type);
            let response_name = proto_type_name(&method.output_proto_type);
            let lease = spec
                .lease_valid_for_ms
                .map_or_else(|| "None".to_owned(), |value| format!("Some({value})"));
            match spec.shape {
                ContractShape::Call => buffer.push_str(&format!(
                    "        pub const {}: ::phoxal::contract::CallMethod<{}, {}> = ::phoxal::contract::CallMethod::new({:?}, {:?}, {:?}, {:?}, {:?}, {}, &super::super::__PHOXAL_DESCRIPTOR_SET);\n",
                    spec.constant_name,
                    input_type,
                    output_type,
                    service_name,
                    method.proto_name,
                    spec.public_name,
                    request_name,
                    response_name,
                    lease,
                )),
                ContractShape::Observation => buffer.push_str(&format!(
                    "        pub const {}: ::phoxal::contract::ObservationMethod<{}> = ::phoxal::contract::ObservationMethod::new({:?}, {:?}, {:?}, {:?}, {:?}, {}, {}, &super::super::__PHOXAL_DESCRIPTOR_SET);\n",
                    spec.constant_name,
                    output_type,
                    service_name,
                    method.proto_name,
                    spec.public_name,
                    request_name,
                    response_name,
                    spec.retained_latest,
                    lease,
                )),
            }
        }
        buffer.push_str("    }\n");
        buffer.push_str("}\n");
    }
}

fn nested_contract_type(rust_type: &str) -> String {
    if rust_type.starts_with("::") || rust_type.starts_with('(') {
        rust_type.to_owned()
    } else {
        format!("super::super::{rust_type}")
    }
}

fn proto_type_name(proto_type: &str) -> String {
    proto_type.trim_start_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use prost::Message as _;
    use prost_reflect::{DescriptorPool, DynamicMessage, Value};
    use prost_types::FileDescriptorSet;

    use super::{
        API_PROTO, DESCRIPTOR_FILE, DependencyDescriptor, Error,
        compile_contracts_to_with_dependencies, compile_to, compile_to_with_dependencies,
        include_dir, merge_dependency_descriptors,
    };

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
        let result = compile_to(&paths, &[source.path()], output.path(), DESCRIPTOR_FILE);
        (source, output, result)
    }

    fn compile_contract_sources(
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
        let result = compile_contracts_to_with_dependencies(
            &paths,
            &[source.path()],
            output.path(),
            &[],
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
    fn packaged_api_options_have_stable_public_identity() {
        assert!(API_PROTO.contains("package phoxal.api;"));
        assert!(API_PROTO.contains("bool retained_latest = 50001;"));
        assert!(API_PROTO.contains("Lease lease = 50002;"));
        assert!(include_dir().join("phoxal/api.proto").is_file());
    }

    #[test]
    fn derives_calls_observations_and_modifiers_from_contract_shape() {
        let (_source, output, result) = compile_contract_sources(&[(
            "example/control/v1/control.proto",
            r#"
                syntax = "proto3";
                package example.control.v1;
                import "google/protobuf/empty.proto";
                import "phoxal/api.proto";

                message SetRequest { double value = 1; }
                message SetResponse { bool accepted = 1; }
                message Status { double value = 1; }

                service Control {
                  rpc Set(SetRequest) returns (SetResponse) {
                    option (phoxal.api.lease) = { valid_for_ms: 125 };
                  }
                  rpc Statuses(google.protobuf.Empty) returns (stream Status) {
                    option (phoxal.api.retained_latest) = true;
                  }
                }
            "#,
        )]);
        result.expect("contract generation");

        let generated = fs::read_to_string(output.path().join("example.control.v1.rs"))
            .expect("generated Rust");
        assert!(generated.contains("pub mod methods"));
        assert!(generated.contains("contract::CallMethod<"));
        assert!(generated.contains("super::super::SetRequest"));
        assert!(generated.contains("super::super::SetResponse"));
        assert!(generated.contains("\"Set\""));
        assert!(generated.contains("\"example.control.v1.SetRequest\""));
        assert!(generated.contains("Some(125)"));
        assert!(generated.contains("contract::ObservationMethod<"));
        assert!(generated.contains("super::super::Status"));
        assert!(generated.contains("\"Statuses\""));
        assert!(generated.contains("\"google.protobuf.Empty\""));
        assert!(generated.contains("true,"));
        assert!(
            !output.path().join("google.protobuf.rs").exists(),
            "contract packages use the canonical phoxal::contract::Empty instead of duplicating well-known types"
        );

        let descriptors = fs::read(output.path().join(DESCRIPTOR_FILE)).expect("descriptors");
        let pool = DescriptorPool::decode(descriptors.as_slice()).expect("descriptor closure");
        assert!(
            pool.get_extension_by_name("phoxal.api.retained_latest")
                .is_some()
        );
        assert!(pool.get_extension_by_name("phoxal.api.lease").is_some());
    }

    #[test]
    fn rejects_invalid_new_contract_shapes_and_modifiers() {
        let (_source, _output, streaming_request) = compile_contract_sources(&[(
            "example/control/v1/control.proto",
            r#"
                syntax = "proto3";
                package example.control.v1;
                message Request {}
                message Response {}
                service Control {
                  rpc Invalid(stream Request) returns (Response);
                }
            "#,
        )]);
        assert!(matches!(
            streaming_request,
            Err(Error::UnsupportedMethodShape { .. })
        ));

        let (_source, _output, retained_call) = compile_contract_sources(&[(
            "example/control/v1/control.proto",
            r#"
                syntax = "proto3";
                package example.control.v1;
                import "phoxal/api.proto";
                message Request {}
                message Response {}
                service Control {
                  rpc Invalid(Request) returns (Response) {
                    option (phoxal.api.retained_latest) = true;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            retained_call,
            Err(Error::InvalidModifier { modifier, .. })
                if modifier == super::RETAINED_LATEST_EXTENSION
        ));
    }

    #[test]
    fn compiles_message_only_contract_without_method_descriptors() {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let proto = source.path().join("vocabulary.proto");
        fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package example.vocabulary.v1;
                message Measurement { optional double value = 1; }
            "#,
        )
        .expect("fixture source");

        compile_to(&[&proto], &[source.path()], output.path(), DESCRIPTOR_FILE)
            .expect("contract generation");

        let generated = fs::read_to_string(output.path().join("example.vocabulary.v1.rs"))
            .expect("generated Rust");
        assert!(generated.contains("pub struct Measurement"));
        // A messages-only owner must not invent a service-method surface.
        assert!(!generated.contains("CallMethod"));
        assert!(!generated.contains("ObservationMethod"));
        assert!(!generated.contains("descriptor_frame"));
        assert!(output.path().join(DESCRIPTOR_FILE).is_file());
    }

    #[test]
    fn imports_dependency_descriptors_without_dependency_sources() {
        let (dependency_source, dependency_output, dependency_result) = compile_sources(&[(
            "example/shared/v1/payload.proto",
            r#"
                syntax = "proto3";
                package example.shared.v1;
                message SharedPayload { uint64 value = 1; }
            "#,
        )]);
        dependency_result.expect("dependency descriptor generation");
        let descriptors = fs::read(dependency_output.path().join(DESCRIPTOR_FILE))
            .expect("dependency descriptors");
        drop(dependency_source);

        let owner_source = tempfile::tempdir().expect("owner source");
        let owner_output = tempfile::tempdir().expect("owner output");
        let owner_proto = owner_source.path().join("example/owner/v1/owner.proto");
        fs::create_dir_all(owner_proto.parent().expect("owner parent")).expect("owner directory");
        fs::write(
            &owner_proto,
            r#"
                syntax = "proto3";
                package example.owner.v1;
                import "example/shared/v1/payload.proto";
                message Owned { example.shared.v1.SharedPayload payload = 1; }
            "#,
        )
        .expect("owner source");

        compile_to_with_dependencies(
            &[&owner_proto],
            &[owner_source.path()],
            owner_output.path(),
            &[DependencyDescriptor::new("example-shared", &descriptors)],
            &[(".example.shared.v1", "::example_shared")],
            DESCRIPTOR_FILE,
        )
        .expect("descriptor-only dependency import");

        let generated = fs::read_to_string(owner_output.path().join("example.owner.v1.rs"))
            .expect("generated owner binding");
        assert!(generated.contains("::example_shared::SharedPayload"));
        let closure = fs::read(owner_output.path().join(DESCRIPTOR_FILE)).expect("owner closure");
        let pool = DescriptorPool::decode(closure.as_slice()).expect("owner descriptor closure");
        assert!(
            pool.get_message_by_name("example.shared.v1.SharedPayload")
                .is_some()
        );
    }

    #[test]
    fn imports_dependency_descriptors_without_source_locations() {
        let (_dependency_source, dependency_output, dependency_result) =
            compile_sources(&[("example/shared/v1/payload.proto", SHARED_PAYLOAD)]);
        dependency_result.expect("dependency descriptor generation");
        let descriptors = fs::read(dependency_output.path().join(DESCRIPTOR_FILE))
            .expect("dependency descriptors");
        let mut stripped = FileDescriptorSet::decode(descriptors.as_slice())
            .expect("dependency descriptor set decodes");
        for file in &mut stripped.file {
            file.source_code_info = None;
        }
        let stripped = stripped.encode_to_vec();

        let owner_source = tempfile::tempdir().expect("owner source");
        let owner_output = tempfile::tempdir().expect("owner output");
        let owner_proto = owner_source.path().join("example/owner/v1/owner.proto");
        fs::create_dir_all(owner_proto.parent().expect("owner parent")).expect("owner directory");
        fs::write(
            &owner_proto,
            r#"
                syntax = "proto3";
                package example.owner.v1;
                import "example/shared/v1/payload.proto";
                message Owned { example.shared.v1.SharedPayload payload = 1; }
            "#,
        )
        .expect("owner source");

        compile_to_with_dependencies(
            &[&owner_proto],
            &[owner_source.path()],
            owner_output.path(),
            &[DependencyDescriptor::new("example-shared", &stripped)],
            &[(".example.shared.v1", "::example_shared")],
            DESCRIPTOR_FILE,
        )
        .expect("source-location-free dependency import");

        let closure = fs::read(owner_output.path().join(DESCRIPTOR_FILE)).expect("owner closure");
        let pool = DescriptorPool::decode(closure.as_slice()).expect("owner descriptor closure");
        assert!(
            pool.get_message_by_name("example.shared.v1.SharedPayload")
                .is_some()
        );
    }

    #[test]
    fn exchanges_an_owned_message_with_an_independent_python_peer() {
        let (_source, output, result) =
            compile_sources(&[("example/shared/v1/payload.proto", SHARED_PAYLOAD)]);
        result.expect("contract generation");
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
    fn accepts_an_identical_transitive_descriptor_diamond() {
        let (_left_source, left_output, left_result) = compile_sources(&[
            (
                "example/shared/v1/shared.proto",
                "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
            ),
            (
                "example/left/v1/left.proto",
                "syntax = \"proto3\"; package example.left.v1; import \"example/shared/v1/shared.proto\"; message Left { example.shared.v1.Shared value = 1; }",
            ),
        ]);
        left_result.expect("left descriptor generation");
        let (_right_source, right_output, right_result) = compile_sources(&[
            (
                "example/shared/v1/shared.proto",
                "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
            ),
            (
                "example/right/v1/right.proto",
                "syntax = \"proto3\"; package example.right.v1; import \"example/shared/v1/shared.proto\"; message Right { example.shared.v1.Shared value = 1; }",
            ),
        ]);
        right_result.expect("right descriptor generation");
        let left = fs::read(left_output.path().join(DESCRIPTOR_FILE)).expect("left descriptors");
        let right = fs::read(right_output.path().join(DESCRIPTOR_FILE)).expect("right descriptors");

        let merged = merge_dependency_descriptors(&[
            DependencyDescriptor::new("example-left", &left),
            DependencyDescriptor::new("example-right", &right),
        ])
        .expect("identical diamond");
        let pool = DescriptorPool::decode(merged.as_slice()).expect("merged descriptor closure");
        assert!(pool.get_message_by_name("example.left.v1.Left").is_some());
        assert!(pool.get_message_by_name("example.right.v1.Right").is_some());
        assert_eq!(
            pool.files()
                .filter(|file| file.name() == "example/shared/v1/shared.proto")
                .count(),
            1
        );
    }

    #[test]
    fn rejects_conflicting_dependency_paths_before_protoc() {
        let (_first_source, first_output, first_result) = compile_sources(&[(
            "example/shared/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared { uint64 value = 1; }",
        )]);
        first_result.expect("first descriptor generation");
        let (_second_source, second_output, second_result) = compile_sources(&[(
            "example/shared/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared { string value = 1; }",
        )]);
        second_result.expect("second descriptor generation");
        let first = fs::read(first_output.path().join(DESCRIPTOR_FILE)).expect("first descriptors");
        let second =
            fs::read(second_output.path().join(DESCRIPTOR_FILE)).expect("second descriptors");

        assert!(matches!(
            merge_dependency_descriptors(&[
                DependencyDescriptor::new("first-owner", &first),
                DependencyDescriptor::new("second-owner", &second),
            ]),
            Err(Error::DescriptorConflict { identity, first, second })
                if identity == "example/shared/v1/shared.proto"
                    && first == "first-owner"
                    && second == "second-owner"
        ));
    }

    #[test]
    fn rejects_conflicting_dependency_symbols_before_protoc() {
        let (_first_source, first_output, first_result) = compile_sources(&[(
            "example/first/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
        )]);
        first_result.expect("first descriptor generation");
        let (_second_source, second_output, second_result) = compile_sources(&[(
            "example/second/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
        )]);
        second_result.expect("second descriptor generation");
        let first = fs::read(first_output.path().join(DESCRIPTOR_FILE)).expect("first descriptors");
        let second =
            fs::read(second_output.path().join(DESCRIPTOR_FILE)).expect("second descriptors");

        assert!(matches!(
            merge_dependency_descriptors(&[
                DependencyDescriptor::new("first-owner", &first),
                DependencyDescriptor::new("second-owner", &second),
            ]),
            Err(Error::DescriptorConflict { identity, first, second })
                if identity == "example.shared.v1.Shared"
                    && first == "first-owner"
                    && second == "second-owner"
        ));
    }

    #[test]
    fn rejects_owned_sources_outside_include_roots() {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let outside = tempfile::NamedTempFile::new().expect("temporary outside source");
        let result = compile_to(
            &[PathBuf::from(outside.path())],
            &[source.path()],
            output.path(),
            DESCRIPTOR_FILE,
        );
        assert!(matches!(result, Err(Error::SourceOutsideIncludes(_))));
    }
}
