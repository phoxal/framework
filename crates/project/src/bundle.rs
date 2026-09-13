//! Deterministic compiled bundle assembly for one prepared robot project.
//!
//! The project compiler owns the source-side assembly boundary. It builds the
//! exact executable targets selected by robot.yaml, records the resolved Cargo
//! identities and authored inputs, and publishes one complete directory
//! atomically. This module deliberately does not launch a process, install a
//! release, or claim that a built executable is Ready.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{self, ArtifactSummary};
use crate::cargo;
use crate::selection::PackageSource;
use crate::validation;
use crate::{CargoOptions, Error, PreparedProject, RobotDocument};

/// The compiled project-bundle schema emitted by this source compiler.
pub const BUNDLE_SCHEMA: &str = "phoxal/bundle/v0";
const BIN_DIR: &str = "bin";
const ASSET_DIR: &str = "assets";
const SOURCE_DIR: &str = "source";
const MANIFEST_FILE: &str = "manifest.json";
const PROVENANCE_FILE: &str = "provenance.json";

/// A complete source-side compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBundle {
    root: PathBuf,
    manifest: BundleManifest,
    provenance: BundleProvenance,
}

impl CompiledBundle {
    /// The published bundle directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The parsed manifest written at manifest.json.
    #[must_use]
    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    /// The parsed provenance written at provenance.json.
    #[must_use]
    pub fn provenance(&self) -> &BundleProvenance {
        &self.provenance
    }

    /// Resolve one selected executable by its bundle-relative instance name.
    #[must_use]
    pub fn executable(&self, instance: &str) -> PathBuf {
        self.root.join(BIN_DIR).join(instance)
    }

    /// Resolve the immutable local source closure carried by the bundle.
    #[must_use]
    pub fn source_root(&self) -> PathBuf {
        self.root.join(SOURCE_DIR)
    }
}

/// The inspectable graph and artifact inventory for one compiled robot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Format discriminator for the compiled bundle.
    pub schema: String,
    /// Authored robot identity.
    pub robot_id: String,
    /// The complete source document used for this compilation.
    pub document: RobotDocument,
    /// Root Cargo package selected as the brain owner.
    pub root_package: BundlePackage,
    /// Cargo target triple or the explicit host marker.
    pub target: String,
    /// Cargo profile used for artifact construction.
    pub profile: String,
    /// Root features selected for this build.
    pub features: Vec<String>,
    /// Every executable selected for this robot, in stable bundle order.
    pub executables: Vec<BundleExecutable>,
    /// Every mounted component, including passive components without a binary.
    pub components: Vec<BundleComponent>,
}

/// A package identity retained in the compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundlePackage {
    /// Cargo package identity, including source and version.
    pub id: String,
    /// Human-readable Cargo package name.
    pub name: String,
    /// Cargo source identity, when the package is not a local workspace member.
    pub source: String,
}

/// One executable copied into bin/ in the compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleExecutable {
    /// brain, service, or component driver.
    pub role: String,
    /// Runtime instance identity used as the bundle filename.
    pub instance: String,
    /// Cargo package identity that produced this executable.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Cargo target name.
    pub target: String,
    /// Bundle-relative executable path.
    pub path: String,
    /// Exact executable byte count.
    pub bytes: u64,
    /// Lowercase SHA-256 digest of the executable bytes.
    pub sha256: String,
    /// Runtime contract and retained descriptor inventory when present.
    pub artifact: Option<BundleArtifact>,
}

/// Exact provenance for the supervisor executable carried by a bundle.
///
/// The supervisor is intentionally kept out of [`BundleManifest::executables`]
/// because that list describes runtime participant processes.  Keeping this
/// record in provenance lets the deployed supervisor continue to interpret
/// the participant list without ever treating its own binary as a child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSupervisor {
    /// Fixed supervisor role label.
    pub role: String,
    /// Fixed bundle-local supervisor instance label.
    pub instance: String,
    /// Cargo package identity that produced the supervisor.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Stable Cargo source identity for the package.
    pub source: String,
    /// Exact Cargo package version.
    pub version: String,
    /// Cargo binary target name.
    pub target: String,
    /// Bundle-relative executable path.
    pub path: String,
    /// Number of bytes in the copied executable.
    pub bytes: u64,
    /// SHA-256 digest of the copied executable.
    pub sha256: String,
}

/// Manifest-safe native artifact contract inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleArtifact {
    /// Runtime timing, configuration, and binding records.
    pub runtime: crate::artifact::RuntimeRecord,
    /// Original descriptor closure digests and file names.
    pub descriptors: Vec<crate::artifact::DescriptorSummary>,
}

impl From<ArtifactSummary> for BundleArtifact {
    fn from(summary: ArtifactSummary) -> Self {
        Self {
            runtime: summary.runtime,
            descriptors: summary.descriptors,
        }
    }
}

/// One mounted component retained in the compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleComponent {
    /// Authored component instance identity.
    pub instance: String,
    /// Exact Cargo dependency key selected by robot.yaml.
    pub dependency_key: String,
    /// Cargo package identity.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Stable Cargo source identity.
    pub source: String,
}

/// The source class of one package in the resolved Cargo closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BundleSourceKind {
    /// A package selected from the robot's local workspace or an external path.
    Local,
    /// A package selected from a pinned Git revision.
    Git,
    /// A package selected from a Cargo registry archive.
    Registry,
    /// A Cargo source not recognized by this version of the tooling.
    Other,
}

/// Immutable provenance for a pinned Git package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleGitSource {
    /// Repository URL without Cargo's source prefix or query string.
    pub repository: String,
    /// Full immutable commit selected by Cargo.
    pub revision: String,
    /// Package subdirectory within the checked-out revision.
    pub subdirectory: String,
}

/// One source file captured as a digest in bundle provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSourceFile {
    /// Path relative to the source package root.
    pub path: String,
    /// Lowercase SHA-256 digest of the source bytes.
    pub sha256: String,
    /// Exact source byte count.
    pub bytes: u64,
}

/// One package and its exact source closure in the resolved Cargo graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSource {
    /// Stable identity that does not contain a developer-local absolute path.
    pub identity: String,
    /// Public package identity used by the bundle, without local path leakage.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Cargo package version.
    pub version: String,
    /// Stable Cargo source representation, or `local` for path packages.
    pub source: String,
    /// Source classification.
    pub kind: BundleSourceKind,
    /// Digest over the sorted source file path and byte closure.
    pub digest: String,
    /// Every regular file in the package source closure.
    pub files: Vec<BundleSourceFile>,
    /// Registry archive checksum when this is a registry package.
    pub registry_checksum: Option<String>,
    /// Git provenance when this is a Git package.
    pub git: Option<BundleGitSource>,
}

/// Toolchain and invocation inputs used to create one compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleToolchain {
    /// Exact Cargo version output.
    pub cargo: String,
    /// Exact verbose rustc version output.
    pub rustc: String,
    /// Target triple or the explicit host marker.
    pub target: String,
    /// Cargo profile selected for the build.
    pub profile: String,
    /// Root features selected for the build.
    pub features: Vec<String>,
    /// Whether the root requested all features.
    pub all_features: bool,
    /// Whether the root disabled default features.
    pub no_default_features: bool,
    /// Cargo lock policy used for the build.
    pub lock: String,
    /// Whether Cargo was forced offline.
    pub offline: bool,
    /// Cargo's caller-provided arguments, retained in invocation order.
    pub cargo_args: Vec<String>,
    /// Cargo message format requested by the caller.
    pub message_format: Option<String>,
    /// Environment inputs that can affect Cargo, Rust, or native builds.
    pub environment: Vec<BundleEnvironment>,
    /// Native tool identities observed in the build environment.
    pub native_tools: Vec<BundleNativeTool>,
    /// Workspace configuration files carried by the source closure.
    pub config_files: Vec<BundleFile>,
    /// Exact Cargo argument vectors used for selected executable builds.
    pub invocations: Vec<BundleCargoInvocation>,
}

/// One environment value retained as build provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEnvironment {
    /// Environment variable name.
    pub name: String,
    /// Environment variable value, or an explicit redaction marker.
    pub value: String,
}

/// One native compiler or linker input retained as build provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleNativeTool {
    /// Environment variable selecting the tool.
    pub name: String,
    /// Configured tool path or command.
    pub command: String,
    /// Version output when the configured command could be queried.
    pub version: Option<String>,
}

/// One Cargo invocation used to produce a selected executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleCargoInvocation {
    /// Cargo arguments in process order, with local roots made relocatable.
    pub arguments: Vec<String>,
}

/// The relocatable local source closure carried by a compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSourceClosure {
    /// Bundle-relative source directory.
    pub path: String,
    /// Digest over every retained source file and its relative path.
    pub digest: String,
    /// Exact file inventory relative to the closure directory.
    pub files: Vec<BundleSourceFile>,
}

/// Source and tool inputs used to construct a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleProvenance {
    /// Format discriminator for the provenance record.
    pub schema: String,
    /// SHA-256 of the authored robot document.
    pub robot_manifest_sha256: String,
    /// SHA-256 of the root Cargo manifest.
    pub cargo_manifest_sha256: String,
    /// SHA-256 of the owning workspace-root Cargo manifest.
    ///
    /// This remains explicit even when the robot package itself is the
    /// workspace root, so inherited workspace package, dependency, profile,
    /// target, and lint settings have a stable provenance field.
    pub cargo_workspace_manifest_sha256: String,
    /// SHA-256 of the workspace-owned Cargo lock, when present.
    pub cargo_lock_sha256: Option<String>,
    /// Exact workspace-owned Cargo.lock input, when present.
    pub cargo_lock: Option<BundleFile>,
    /// Deduplicated package source closure used by the selected Cargo graph.
    pub sources: Vec<BundleSource>,
    /// Digest over the ordered source records.
    pub source_closure_sha256: String,
    /// Relocatable local source and lock closure carried by the bundle.
    pub source_tree: BundleSourceClosure,
    /// Compiler and invocation inputs used for the bundle.
    pub toolchain: BundleToolchain,
    /// The exact supervisor executable copied into the bundle and launched by
    /// local execution.
    pub supervisor: BundleSupervisor,
    /// Model path and digest when the authored model exists.
    pub model: Option<BundleFile>,
    /// The validated model/resource closure copied into the bundle's assets.
    pub model_closure: Option<BundleModelClosure>,
}

/// One authored input file and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    /// Path relative to the robot root.
    pub path: String,
    /// Lowercase SHA-256 digest of the file bytes.
    pub sha256: String,
    /// Exact byte count.
    pub bytes: u64,
}

/// The portable closed model closure carried by a compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleModelClosure {
    /// Bundle-relative entry passed to the native parser.
    pub entry: String,
    /// Digest of the normalized entry/resource closure.
    pub digest: String,
    /// Every resource copied below the bundle's assets directory.
    pub resources: Vec<BundleResource>,
}

/// One resource copied into a compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleResource {
    /// Bundle-relative resource path.
    pub path: String,
    /// Lowercase SHA-256 digest of the resource bytes.
    pub sha256: String,
    /// Exact resource byte count.
    pub bytes: u64,
}

#[derive(Debug, Clone)]
struct StagedModel {
    source: BundleFile,
    closure: BundleModelClosure,
}

#[derive(Debug, Clone)]
struct StagedResource {
    name: String,
    bytes: Vec<u8>,
}

/// A local identity used by the explicit run and simulation boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIdentity {
    scope: String,
    supervisor_id: String,
}

impl LocalIdentity {
    /// Construct a bounded local namespace identity.
    pub fn new(scope: impl Into<String>, supervisor_id: impl Into<String>) -> Result<Self, Error> {
        let scope = scope.into();
        let supervisor_id = supervisor_id.into();
        validate_identity_part("scope", &scope)?;
        validate_identity_part("supervisor_id", &supervisor_id)?;
        Ok(Self {
            scope,
            supervisor_id,
        })
    }

    /// Router namespace.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Supervisor identity within that namespace.
    #[must_use]
    pub fn supervisor_id(&self) -> &str {
        &self.supervisor_id
    }
}

/// A locally prepared hardware launch.
///
/// The plan contains no process handle and no readiness claim. The supervisor
/// host owns actual process startup and admission in a later slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRunPlan {
    /// Compiled bundle selected for launch.
    pub bundle: CompiledBundle,
    /// Local router namespace.
    pub identity: LocalIdentity,
}

/// A locally prepared simulation launch.
///
/// The independent simulator application is intentionally not provisioned or
/// launched by this first project-delivery slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSimulationPlan {
    /// Compiled robot bundle selected for the simulation.
    pub bundle: CompiledBundle,
    /// Local router namespace.
    pub identity: LocalIdentity,
}

pub(crate) fn assemble_with_inputs(
    prepared: &PreparedProject,
    options: &CargoOptions,
    output: impl AsRef<Path>,
    expected_inputs: Option<&BuildInputs>,
) -> Result<CompiledBundle, Error> {
    options.validate()?;
    let output = output.as_ref();
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| Error::BundleDirectory {
        path: parent.to_owned(),
        source,
    })?;
    let _publication_lock = acquire_bundle_publication_lock(output)?;

    let staging = tempfile::Builder::new()
        .prefix(".phoxal-bundle-")
        .tempdir_in(parent)
        .map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    let staged_root = staging.path();
    let build_inputs = expected_inputs
        .cloned()
        .map_or_else(|| capture_build_inputs(prepared), Ok)?;
    fs::create_dir(staged_root.join(BIN_DIR)).map_err(|source| Error::BundleDirectory {
        path: staged_root.join(BIN_DIR),
        source,
    })?;
    let staged_model = stage_model(prepared, staged_root)?;
    let staged_source_tree = stage_source_tree(prepared, staged_root)?;

    let mut artifacts = BTreeMap::new();
    let mut invocations = Vec::new();
    for (_, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        if artifacts.contains_key(&key) {
            continue;
        }
        let output = cargo::build_target(prepared, target, options)?;
        invocations.push(output.arguments.clone());
        let executable = cargo::artifact_path(&output.stdout, target)?;
        ensure_regular_executable(&executable, &target.target)?;
        let digest = digest_file(&executable)?;
        let instance = prepared
            .assembly_targets()
            .into_iter()
            .find(|(_, selected)| {
                selected.package_id == target.package_id && selected.target == target.target
            })
            .map(|(instance, _)| instance)
            .unwrap_or_else(|| target.target.clone());
        let role = prepared.executable_role(&instance);
        let contract = artifact::inspect_file(&executable).map_err(|error| {
            if matches!(error, artifact::Error::MissingRecord) {
                Error::MissingArtifactContract {
                    role: role.clone(),
                    instance: instance.clone(),
                    package: target.package.clone(),
                    target: target.target.clone(),
                }
            } else {
                Error::ArtifactInvalid {
                    path: executable.clone(),
                    message: error.to_string(),
                }
            }
        })?;
        artifacts.insert(key, (target.clone(), executable, digest, contract));
    }

    let mut executable_records = Vec::new();
    for (instance, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        let (built_target, source, digest, contract) =
            artifacts.get(&key).ok_or_else(|| Error::ArtifactCapture {
                package: target.package.clone(),
                target: target.target.clone(),
                message: "Cargo did not produce a selected executable".to_owned(),
            })?;
        let destination_name = safe_bundle_name(&instance)?;
        let relative = format!("{BIN_DIR}/{destination_name}");
        let destination = staged_root.join(&relative);
        copy_executable(source, &destination)?;
        executable_records.push(BundleExecutable {
            role: prepared.executable_role(&instance),
            instance: instance.clone(),
            package_id: public_package_id(prepared, &built_target.package_id),
            package: built_target.package.clone(),
            target: built_target.target.clone(),
            path: relative,
            bytes: digest.bytes,
            sha256: digest.sha256.clone(),
            artifact: Some(contract.summary().into()),
        });
    }
    executable_records.sort_by(|left, right| left.path.cmp(&right.path));

    // The supervisor is part of the immutable bundle, but it is not a runtime
    // participant.  Keep it out of manifest.executables because the deployed
    // supervisor must never interpret its own binary as a child process.
    let supervisor_output = cargo::build_target(prepared, &prepared.sources().supervisor, options)?;
    invocations.push(supervisor_output.arguments.clone());
    let supervisor_source =
        cargo::artifact_path(&supervisor_output.stdout, &prepared.sources().supervisor)?;
    ensure_regular_executable(&supervisor_source, "supervisor")?;
    let supervisor_digest = digest_file(&supervisor_source)?;

    let contract_map = artifacts
        .iter()
        .map(|(key, (_, _, _, contract))| (key.clone(), contract.clone()))
        .collect::<BTreeMap<_, _>>();
    validation::validate_configurations(prepared, &contract_map)?;
    validation::validate_connections(prepared, &contract_map)?;
    verify_build_inputs(prepared, &build_inputs)?;

    let mut components = prepared
        .sources()
        .components
        .values()
        .map(|component| {
            Ok(BundleComponent {
                instance: component.instance.clone(),
                dependency_key: component.dependency_key.clone(),
                package_id: public_package_id(prepared, &component.package_id),
                package: component.package.clone(),
                source: source_identity(&component.source)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    components.sort_by(|left, right| left.instance.cmp(&right.instance));

    let (sources, source_closure_sha256) = source_closure(prepared)?;
    let supervisor_source_record = sources
        .iter()
        .find(|source| {
            source.package_id
                == public_package_id(prepared, &prepared.sources().supervisor.package_id)
        })
        .ok_or_else(|| Error::ArtifactInvalid {
            path: supervisor_source.clone(),
            message: "supervisor package has no retained source provenance".to_owned(),
        })?;
    let supervisor_relative = format!("{BIN_DIR}/supervisor");
    copy_executable(&supervisor_source, &staged_root.join(&supervisor_relative))?;
    let supervisor = BundleSupervisor {
        role: "supervisor".to_owned(),
        instance: "supervisor".to_owned(),
        package_id: supervisor_source_record.package_id.clone(),
        package: prepared.sources().supervisor.package.clone(),
        source: supervisor_source_record.source.clone(),
        version: supervisor_source_record.version.clone(),
        target: prepared.sources().supervisor.target.clone(),
        path: supervisor_relative,
        bytes: supervisor_digest.bytes,
        sha256: supervisor_digest.sha256,
    };
    let target = effective_target(options);
    let profile = effective_profile(options);
    let features = effective_features(options);
    let manifest = BundleManifest {
        schema: BUNDLE_SCHEMA.to_owned(),
        robot_id: prepared.document().robot.id.clone(),
        document: prepared.document().clone(),
        root_package: BundlePackage {
            id: public_package_id(prepared, &prepared.root_package().id.to_string()),
            name: prepared.root_package().name.to_string(),
            source: "local".to_owned(),
        },
        target,
        profile,
        features,
        executables: executable_records,
        components,
    };
    let provenance = provenance(
        prepared,
        staged_model.as_ref(),
        sources,
        source_closure_sha256,
        staged_source_tree,
        options,
        &manifest,
        &invocations,
        supervisor,
    )?;
    write_json(&staged_root.join(MANIFEST_FILE), &manifest)?;
    write_json(&staged_root.join(PROVENANCE_FILE), &provenance)?;

    let staged = staging.keep();
    publish_directory(&staged, output)?;
    Ok(CompiledBundle {
        root: output.to_owned(),
        manifest,
        provenance,
    })
}

#[allow(clippy::too_many_arguments)]
fn provenance(
    prepared: &PreparedProject,
    staged_model: Option<&StagedModel>,
    sources: Vec<BundleSource>,
    source_closure_sha256: String,
    staged_source_tree: BundleSourceClosure,
    options: &CargoOptions,
    manifest: &BundleManifest,
    invocations: &[Vec<std::ffi::OsString>],
    supervisor: BundleSupervisor,
) -> Result<BundleProvenance, Error> {
    let cargo_lock = optional_file(&prepared.cargo_lock(), "Cargo.lock")?;
    let workspace_manifest = prepared.cargo_workspace_root().join("Cargo.toml");
    let toolchain = toolchain(
        options,
        manifest,
        &staged_source_tree,
        invocations,
        prepared.layout().root(),
    )?;
    Ok(BundleProvenance {
        schema: BUNDLE_SCHEMA.to_owned(),
        robot_manifest_sha256: digest_file(prepared.layout().robot_manifest())?.sha256,
        cargo_manifest_sha256: digest_file(prepared.layout().cargo_manifest())?.sha256,
        cargo_workspace_manifest_sha256: digest_file(&workspace_manifest)?.sha256,
        cargo_lock_sha256: cargo_lock.as_ref().map(|file| file.sha256.clone()),
        cargo_lock,
        sources,
        source_closure_sha256,
        source_tree: staged_source_tree,
        toolchain,
        supervisor,
        model: staged_model.map(|model| model.source.clone()),
        model_closure: staged_model.map(|model| model.closure.clone()),
    })
}

fn effective_target(options: &CargoOptions) -> String {
    cargo_arg_value(&options.cargo_args, "--target")
        .or_else(|| options.target.clone())
        .unwrap_or_else(|| "host".to_owned())
}

fn effective_profile(options: &CargoOptions) -> String {
    let mut profile = options.profile.clone();
    if options.release {
        profile = Some("release".to_owned());
    }
    let arguments = options
        .cargo_args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--release" | "-r" => profile = Some("release".to_owned()),
            "--profile" => {
                if let Some(value) = arguments.get(index + 1) {
                    profile = Some(value.clone());
                    index += 1;
                }
            }
            value if value.starts_with("--profile=") => {
                profile = Some(value["--profile=".len()..].to_owned());
            }
            _ => {}
        }
        index += 1;
    }
    profile.unwrap_or_else(|| "dev".to_owned())
}

fn effective_features(options: &CargoOptions) -> Vec<String> {
    let mut features = options.features.clone();
    let arguments = options
        .cargo_args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--features" {
            index += 1;
            while let Some(value) = arguments.get(index) {
                if value.starts_with('-') {
                    break;
                }
                features.extend(
                    value
                        .split(|character: char| character == ',' || character.is_whitespace())
                        .filter(|feature| !feature.is_empty())
                        .map(str::to_owned),
                );
                index += 1;
            }
            continue;
        }
        if let Some(value) = arguments[index].strip_prefix("--features=") {
            features.extend(
                value
                    .split(|character: char| character == ',' || character.is_whitespace())
                    .filter(|feature| !feature.is_empty())
                    .map(str::to_owned),
            );
        }
        index += 1;
    }
    features.sort();
    features.dedup();
    features
}

fn cargo_arg_value(arguments: &[std::ffi::OsString], name: &str) -> Option<String> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let equals = format!("{name}=");
    let mut value = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == name {
            value = arguments.get(index + 1).cloned();
            index += 1;
        } else if let Some(argument) = arguments[index].strip_prefix(&equals) {
            value = Some(argument.to_owned());
        }
        index += 1;
    }
    value
}

fn toolchain(
    options: &CargoOptions,
    manifest: &BundleManifest,
    source_tree: &BundleSourceClosure,
    invocations: &[Vec<std::ffi::OsString>],
    project_root: &Path,
) -> Result<BundleToolchain, Error> {
    let cargo = options.cargo_program();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    Ok(BundleToolchain {
        cargo: version_output(cargo.as_os_str(), &["--version"])?
            .trim_end()
            .to_owned(),
        rustc: version_output(&rustc, &["-vV"])?.trim_end().to_owned(),
        target: manifest.target.clone(),
        profile: manifest.profile.clone(),
        features: manifest.features.clone(),
        all_features: effective_all_features(options),
        no_default_features: effective_no_default_features(options),
        lock: effective_lock(options),
        offline: options.offline || has_cargo_flag(options, "--offline"),
        cargo_args: options
            .cargo_args
            .iter()
            .map(|argument| normalize_provenance_text(&argument.to_string_lossy(), project_root))
            .collect(),
        message_format: Some(
            options
                .message_format
                .as_deref()
                .filter(|format| format.starts_with("json"))
                .unwrap_or("json-render-diagnostics")
                .to_owned(),
        ),
        environment: build_environment(),
        native_tools: native_tools(),
        config_files: {
            let mut files = source_tree_config_files(source_tree);
            files.extend(external_cargo_config_files()?);
            files
        },
        invocations: invocations
            .iter()
            .map(|arguments| BundleCargoInvocation {
                arguments: arguments
                    .iter()
                    .map(|argument| {
                        normalize_provenance_text(&argument.to_string_lossy(), project_root)
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn has_cargo_flag(options: &CargoOptions, flag: &str) -> bool {
    options
        .cargo_args
        .iter()
        .any(|argument| argument.to_string_lossy() == flag)
}

fn effective_all_features(options: &CargoOptions) -> bool {
    options.all_features || has_cargo_flag(options, "--all-features")
}

fn effective_no_default_features(options: &CargoOptions) -> bool {
    options.no_default_features || has_cargo_flag(options, "--no-default-features")
}

fn effective_lock(options: &CargoOptions) -> String {
    if has_cargo_flag(options, "--frozen") {
        "frozen".to_owned()
    } else if has_cargo_flag(options, "--locked") {
        "locked".to_owned()
    } else {
        format!("{:?}", options.lock).to_lowercase()
    }
}

fn version_output(program: &std::ffi::OsStr, arguments: &[&str]) -> Result<String, Error> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|source| Error::ArtifactFile {
            path: PathBuf::from(program),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::ArtifactInvalid {
            path: PathBuf::from(program),
            message: format!(
                "toolchain command failed with {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn external_cargo_config_paths() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        roots.push(PathBuf::from(cargo_home));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cargo"));
    }
    let mut paths = Vec::new();
    for root in roots {
        for name in ["config.toml", "config"] {
            paths.push((format!("external/.cargo/{name}"), root.join(name)));
        }
    }
    paths
}

fn external_cargo_config_files() -> Result<Vec<BundleFile>, Error> {
    external_cargo_config_paths()
        .into_iter()
        .filter(|(_, path)| path.is_file())
        .map(|(path, source)| {
            let digest = digest_file(&source)?;
            Ok(BundleFile {
                path,
                sha256: digest.sha256,
                bytes: digest.bytes,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildInputs {
    source: String,
    workspace_manifest: String,
    environment: String,
}

pub(crate) fn capture_build_inputs(prepared: &PreparedProject) -> Result<BuildInputs, Error> {
    let mut hasher = Sha256::new();
    let mut add_file = |label: String, path: &Path| -> Result<(), Error> {
        let digest = digest_file(path)?;
        update_digest_bytes(&mut hasher, label.as_bytes());
        update_digest_bytes(&mut hasher, digest.sha256.as_bytes());
        update_digest_bytes(&mut hasher, &digest.bytes.to_le_bytes());
        Ok(())
    };
    add_file("robot.yaml".to_owned(), prepared.layout().robot_manifest())?;
    add_file("Cargo.toml".to_owned(), prepared.layout().cargo_manifest())?;
    add_file("Cargo.lock".to_owned(), &prepared.cargo_lock())?;
    let workspace_root = prepared
        .cargo_workspace_root()
        .canonicalize()
        .map_err(|source| Error::ArtifactFile {
            path: prepared.cargo_workspace_root().to_owned(),
            source,
        })?;
    let workspace_manifest = workspace_root.join("Cargo.toml");
    let workspace_manifest_digest = digest_file(&workspace_manifest)?;
    if workspace_manifest != prepared.layout().cargo_manifest() {
        add_file("workspace/Cargo.toml".to_owned(), &workspace_manifest)?;
    }
    if workspace_root.join(".cargo").is_dir() {
        for file in source_files(&workspace_root.join(".cargo"))? {
            add_file(
                format!("workspace/.cargo/{}", file.path),
                &workspace_root.join(".cargo").join(&file.path),
            )?;
        }
    }
    for (label, path) in external_cargo_config_paths() {
        if path.is_file() {
            add_file(label, &path)?;
        }
    }
    for package in prepared
        .metadata()
        .packages
        .iter()
        .filter(|package| resolved_package_ids(prepared).contains(&package.id.to_string()))
    {
        let package_root = package_root(package)?;
        for file in source_files_with(&package_root, package.source.is_some())? {
            add_file(
                format!("package/{}/{}", package.id, file.path),
                &package_root.join(&file.path),
            )?;
        }
    }
    let source = format!("{:x}", hasher.finalize());
    let environment = environment_digest();
    Ok(BuildInputs {
        source,
        workspace_manifest: workspace_manifest_digest.sha256,
        environment,
    })
}

pub(crate) fn verify_build_inputs(
    prepared: &PreparedProject,
    expected: &BuildInputs,
) -> Result<(), Error> {
    let current = capture_build_inputs(prepared)?;
    if current == *expected {
        return Ok(());
    }
    let mut differences = Vec::new();
    if current.source != expected.source {
        differences.push("source, manifest, lock, or workspace configuration");
    }
    if current.workspace_manifest != expected.workspace_manifest {
        differences.push("owning workspace Cargo.toml");
    }
    if current.environment != expected.environment {
        differences.push("build environment");
    }
    Err(Error::BundleSourceChanged {
        message: differences.join(" and "),
    })
}

fn environment_digest() -> String {
    let mut variables = build_environment_values();
    variables.sort();
    let mut hasher = Sha256::new();
    for (name, value) in variables {
        update_digest_bytes(&mut hasher, name.as_bytes());
        update_digest_bytes(&mut hasher, value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn build_environment() -> Vec<BundleEnvironment> {
    render_build_environment(build_environment_values())
}

fn render_build_environment(variables: Vec<(String, String)>) -> Vec<BundleEnvironment> {
    let mut variables = variables
        .into_iter()
        .map(|(name, value)| {
            let value = if is_sensitive_environment_name(&name)
                || is_sensitive_environment_value(&value)
                || looks_like_local_path_value(&name, &value)
            {
                format!("<sha256:{}>", digest_text(&value))
            } else {
                value
            };
            BundleEnvironment { name, value }
        })
        .collect::<Vec<_>>();
    variables.sort_by(|left, right| left.name.cmp(&right.name));
    variables
}

fn build_environment_values() -> Vec<(String, String)> {
    select_build_environment(std::env::vars_os().map(|(name, value)| {
        (
            name.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        )
    }))
}

fn select_build_environment(
    variables: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    variables
        .into_iter()
        .filter(|(name, _)| is_build_environment_name(name))
        .collect()
}

fn is_build_environment_name(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "ANDROID_HOME",
        "ANDROID_NDK_ROOT",
        "AR",
        "BINDGEN_EXTRA_CLANG_ARGS",
        "CC",
        "CARGO_BUILD_INCREMENTAL",
        "CARGO_BUILD_JOBS",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTDOC",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_HOME",
        "CARGO_INCREMENTAL",
        "CARGO_HTTP_CAINFO",
        "CARGO_HTTP_PROXY",
        "CARGO_HTTP_TIMEOUT",
        "CARGO_MAKEFLAGS",
        "CARGO_NET_GIT_FETCH_WITH_CLI",
        "CARGO_NET_OFFLINE",
        "CARGO_REGISTRY_TOKEN",
        "CMAKE",
        "CUDA_HOME",
        "CUDA_PATH",
        "EMSDK",
        "GIT_ASKPASS",
        "GIT_CONFIG_PARAMETERS",
        "GIT_SSH_COMMAND",
        "LD",
        "LIBCLANG_PATH",
        "MACOSX_DEPLOYMENT_TARGET",
        "MAKE",
        "MAKEFLAGS",
        "NASM",
        "NUM_JOBS",
        "OPENSSL_DIR",
        "OPENSSL_INCLUDE_DIR",
        "OPENSSL_LIB_DIR",
        "PKG_CONFIG",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_SYSROOT_DIR",
        "PROTOC",
        "PROTOC_INCLUDE",
        "RANLIB",
        "ROCM_PATH",
        "RUSTC",
        "RUSTC_BOOTSTRAP",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
        "RUSTDOC",
        "RUSTDOCFLAGS",
        "RUSTFLAGS",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "SDKROOT",
        "SSH_AGENT_PID",
        "SSH_AUTH_SOCK",
        "SOURCE_DATE_EPOCH",
        "VCPKG_ROOT",
        "WASI_SDK_PATH",
        "ZIG",
    ];
    NAMES.contains(&name)
        || (name.starts_with("CARGO_REGISTRIES_")
            && ["_INDEX", "_PROTOCOL", "_TOKEN", "_CREDENTIAL_PROVIDER"]
                .iter()
                .any(|suffix| name.ends_with(suffix)))
        || (name.starts_with("CARGO_TARGET_")
            && ["_AR", "_LINKER", "_RUSTC", "_RUSTFLAGS", "_RUNNER"]
                .iter()
                .any(|suffix| name.ends_with(suffix)))
}

fn digest_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn is_sensitive_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "API_KEY",
        "AUTH",
        "AWS",
        "COOKIE",
        "CREDENTIAL",
        "PASSWORD",
        "PRIVATE",
        "SECRET",
        "SESSION",
        "TOKEN",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

fn is_sensitive_environment_value(value: &str) -> bool {
    ["http://", "https://", "ssh://", "git://"]
        .iter()
        .filter_map(|scheme| value.find(scheme).map(|index| index + scheme.len()))
        .any(|authority_start| {
            value[authority_start..].split_once('/').map_or_else(
                || value[authority_start..].contains('@'),
                |(authority, _)| authority.contains('@'),
            )
        })
}

fn looks_like_local_path_value(name: &str, value: &str) -> bool {
    const PATH_NAMES: &[&str] = &[
        "ANDROID_HOME",
        "ANDROID_NDK_ROOT",
        "AR",
        "CC",
        "CARGO_HOME",
        "CARGO_HTTP_CAINFO",
        "CMAKE",
        "CUDA_HOME",
        "CUDA_PATH",
        "LD",
        "LIBCLANG_PATH",
        "MAKE",
        "NASM",
        "OPENSSL_DIR",
        "OPENSSL_INCLUDE_DIR",
        "OPENSSL_LIB_DIR",
        "PKG_CONFIG",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_SYSROOT_DIR",
        "PROTOC",
        "PROTOC_INCLUDE",
        "RANLIB",
        "ROCM_PATH",
        "RUSTC",
        "RUSTDOC",
        "RUSTUP_HOME",
        "SDKROOT",
        "SSH_AUTH_SOCK",
        "VCPKG_ROOT",
        "WASI_SDK_PATH",
        "ZIG",
    ];
    PATH_NAMES.contains(&name) || value.split_whitespace().any(is_local_path_fragment)
}

fn is_local_path_fragment(part: &str) -> bool {
    part.starts_with('/')
        || part.starts_with("\\\\")
        || part.as_bytes().get(1) == Some(&b':')
        || part
            .split_once('=')
            .is_some_and(|(_, suffix)| is_local_path_fragment(suffix))
        || part.starts_with("file:")
}

fn normalize_provenance_text(value: &str, project_root: &Path) -> String {
    let project_root = project_root.display().to_string();
    let current_dir = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string());
    value
        .split_whitespace()
        .map(|part| {
            let part = part.replace(&project_root, "<project-root>");
            let part = current_dir.as_ref().map_or(part.clone(), |current_dir| {
                part.replace(current_dir, "<current-dir>")
            });
            normalize_provenance_token(&part)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_provenance_token(value: &str) -> String {
    let value = normalize_url_credentials(value);
    if value.starts_with('/') || value.starts_with("\\\\") || value.as_bytes().get(1) == Some(&b':')
    {
        return "<local-path>".to_owned();
    }
    if let Some((prefix, suffix)) = value.split_once('=')
        && (suffix.starts_with('/')
            || suffix.starts_with("\\\\")
            || suffix.as_bytes().get(1) == Some(&b':')
            || suffix.starts_with("file:"))
    {
        return format!("{prefix}=<local-path>");
    }
    if value.starts_with("file:") {
        return "file:<local-path>".to_owned();
    }
    if !value.contains("://")
        && let Some(index) = value.find('/')
        && value.starts_with('-')
    {
        return format!("{}<local-path>", &value[..index]);
    }
    value.to_owned()
}

fn normalize_url_credentials(value: &str) -> String {
    for scheme in ["http://", "https://", "ssh://", "git://"] {
        let Some(scheme_start) = value.find(scheme) else {
            continue;
        };
        let authority_start = scheme_start + scheme.len();
        let authority_end = value[authority_start..]
            .find(['/', '?', '#'])
            .map_or(value.len(), |offset| authority_start + offset);
        let authority = &value[authority_start..authority_end];
        let Some(credentials_end) = authority.find('@') else {
            continue;
        };
        return format!(
            "{}<redacted>@{}{}",
            &value[..authority_start],
            &authority[credentials_end + 1..],
            &value[authority_end..]
        );
    }
    if let (Some(at), Some(colon)) = (value.find('@'), value.find(':'))
        && at < colon
    {
        return format!("<redacted>@{}", &value[at + 1..]);
    }
    value.to_owned()
}

fn native_tools() -> Vec<BundleNativeTool> {
    let mut names = BTreeSet::new();
    for name in ["CC", "CXX", "AR", "RANLIB", "LD", "CMAKE", "NASM"] {
        names.insert(name.to_owned());
    }
    for name in std::env::vars_os().map(|(name, _)| name.to_string_lossy().into_owned()) {
        if name.starts_with("CARGO_TARGET_") && name.ends_with("_LINKER") {
            names.insert(name);
        }
    }
    names
        .into_iter()
        .filter_map(|name| {
            let command = std::env::var_os(&name)?;
            let command_text = command.to_string_lossy().into_owned();
            let program = command_text.split_whitespace().next()?;
            let version = Command::new(program)
                .arg("--version")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| {
                    normalize_provenance_text(
                        String::from_utf8_lossy(&output.stdout).trim(),
                        Path::new("<project-root>"),
                    )
                })
                .filter(|value| !value.is_empty());
            Some(BundleNativeTool {
                name,
                command: normalize_provenance_text(&command_text, Path::new("<project-root>")),
                version,
            })
        })
        .collect()
}

fn source_tree_config_files(source_tree: &BundleSourceClosure) -> Vec<BundleFile> {
    source_tree
        .files
        .iter()
        .filter(|file| file.path == ".cargo" || file.path.starts_with(".cargo/"))
        .map(|file| BundleFile {
            path: format!("source/{}", file.path),
            sha256: file.sha256.clone(),
            bytes: file.bytes,
        })
        .collect()
}

fn stage_source_tree(
    prepared: &PreparedProject,
    staged_root: &Path,
) -> Result<BundleSourceClosure, Error> {
    let closure_root = staged_root.join(SOURCE_DIR);
    fs::create_dir_all(&closure_root).map_err(|source| Error::BundleDirectory {
        path: closure_root.clone(),
        source,
    })?;
    let workspace_root = prepared
        .cargo_workspace_root()
        .canonicalize()
        .map_err(|source| Error::ArtifactFile {
            path: prepared.cargo_workspace_root().to_owned(),
            source,
        })?;
    if workspace_root.join(".cargo").is_dir() {
        validate_cargo_config(&workspace_root.join(".cargo"))?;
        copy_source_tree(&workspace_root.join(".cargo"), &closure_root.join(".cargo"))?;
    }
    let package_ids = resolved_package_ids(prepared);
    let local_packages = prepared
        .metadata()
        .packages
        .iter()
        .filter(|package| package_ids.contains(&package.id.to_string()) && package.source.is_none())
        .collect::<Vec<_>>();
    let mut locations = BTreeMap::new();
    let mut workspace_groups = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    for package in &local_packages {
        let package_root = package_root(package)?;
        let owner = if package_root.starts_with(&workspace_root) {
            workspace_root.clone()
        } else {
            owning_workspace_root(&package_root)?
        };
        workspace_groups
            .entry(owner)
            .or_default()
            .push(package_root);
    }
    let mut staged_workspaces = BTreeMap::<PathBuf, (PathBuf, toml::Value)>::new();
    for (owner, packages) in &mut workspace_groups {
        packages.sort();
        packages.dedup();
        let staged_root = if owner == &workspace_root {
            closure_root.clone()
        } else {
            let digest = digest_source_tree(owner)?;
            closure_root.join("_phoxal_path_dependencies").join(digest)
        };
        let owner_manifest = owner.join("Cargo.toml");
        let mut owner_value = read_toml_file(&owner_manifest)?;
        let has_workspace = owner_value
            .get("workspace")
            .is_some_and(toml::Value::is_table);
        if has_workspace {
            let members = packages
                .iter()
                .filter_map(|package| {
                    package
                        .strip_prefix(owner)
                        .ok()
                        .filter(|relative| !relative.as_os_str().is_empty())
                        .map(path_string)
                })
                .collect::<BTreeSet<_>>();
            let workspace = owner_value
                .get_mut("workspace")
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| Error::ArtifactInvalid {
                    path: owner_manifest.clone(),
                    message: "workspace manifest changed shape while staging".to_owned(),
                })?;
            workspace.insert(
                "members".to_owned(),
                toml::Value::Array(members.into_iter().map(toml::Value::String).collect()),
            );
            if owner == &workspace_root {
                let excludes = workspace
                    .entry("exclude".to_owned())
                    .or_insert_with(|| toml::Value::Array(Vec::new()))
                    .as_array_mut()
                    .ok_or_else(|| Error::ArtifactInvalid {
                        path: owner_manifest.clone(),
                        message: "workspace exclude changed shape while staging".to_owned(),
                    })?;
                if !excludes
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|exclude| exclude == "_phoxal_path_dependencies")
                {
                    excludes.push(toml::Value::String("_phoxal_path_dependencies".to_owned()));
                }
            }
            workspace.remove("default-members");
        }
        locations.insert(owner.clone(), staged_root.clone());
        for package in packages.iter() {
            let relative = package
                .strip_prefix(owner)
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::new());
            locations.insert(package.clone(), staged_root.join(relative));
        }
        staged_workspaces.insert(owner.clone(), (staged_root, owner_value));
    }
    for (owner, (staged_root, workspace_value)) in &staged_workspaces {
        if owner.join(".cargo").is_dir() {
            validate_cargo_config(&owner.join(".cargo"))?;
            copy_source_tree(&owner.join(".cargo"), &staged_root.join(".cargo"))?;
        }
        if owner.join("Cargo.lock").is_file() && owner != &workspace_root {
            copy_source_file(&owner.join("Cargo.lock"), &staged_root.join("Cargo.lock"))?;
        }
        let package_roots = workspace_groups
            .get(owner)
            .ok_or_else(|| Error::ArtifactInvalid {
                path: owner.clone(),
                message: "staged workspace has no captured packages".to_owned(),
            })?;
        for original_root in package_roots {
            let staged_package =
                locations
                    .get(original_root)
                    .ok_or_else(|| Error::ArtifactInvalid {
                        path: original_root.clone(),
                        message: "local Cargo source has no staged location".to_owned(),
                    })?;
            if original_root.join(".cargo").is_dir() {
                validate_cargo_config(&original_root.join(".cargo"))?;
            }
            copy_source_tree(original_root, staged_package)?;
            let original_manifest = original_root.join("Cargo.toml");
            let staged_manifest = staged_package.join("Cargo.toml");
            let mut value = read_toml_file(&original_manifest)?;
            let changed = rewrite_local_paths(&mut value, original_root, owner, &locations)?;
            if changed {
                write_toml_file(&staged_manifest, &value)?;
            }
        }
        if workspace_value
            .get("workspace")
            .is_some_and(toml::Value::is_table)
        {
            let mut workspace_value = workspace_value.clone();
            rewrite_workspace_paths(&mut workspace_value, owner, staged_root, &locations)?;
            write_toml_file(&staged_root.join("Cargo.toml"), &workspace_value)?;
        }
    }
    let lock = prepared.cargo_lock();
    if !lock.is_file() {
        return Err(Error::ArtifactInvalid {
            path: lock,
            message: "prepared Cargo graph has no workspace Cargo.lock".to_owned(),
        });
    }
    copy_source_file(&prepared.cargo_lock(), &closure_root.join("Cargo.lock"))?;

    let files = source_files(&closure_root)?;
    Ok(BundleSourceClosure {
        path: SOURCE_DIR.to_owned(),
        digest: digest_source_files(&files),
        files,
    })
}

fn package_root(package: &cargo_metadata::Package) -> Result<PathBuf, Error> {
    PathBuf::from(package.manifest_path.as_std_path())
        .parent()
        .ok_or_else(|| Error::ArtifactInvalid {
            path: PathBuf::from(package.manifest_path.as_std_path()),
            message: "Cargo package manifest has no parent directory".to_owned(),
        })
        .and_then(|path| {
            path.canonicalize().map_err(|source| Error::ArtifactFile {
                path: path.to_owned(),
                source,
            })
        })
}

fn owning_workspace_root(package_root: &Path) -> Result<PathBuf, Error> {
    let manifest = package_root.join("Cargo.toml");
    let value = read_toml_file(&manifest)?;
    if let Some(workspace) = value
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("workspace"))
        .and_then(toml::Value::as_str)
    {
        let root = resolve_local_dependency(package_root, workspace)?;
        if root.join("Cargo.toml").is_file() {
            return root
                .canonicalize()
                .map_err(|source| Error::ArtifactFile { path: root, source });
        }
        return Err(Error::ArtifactInvalid {
            path: manifest,
            message: format!("declared Cargo workspace '{workspace}' has no Cargo.toml"),
        });
    }
    if value.get("workspace").is_some_and(toml::Value::is_table) {
        return package_root
            .canonicalize()
            .map_err(|source| Error::ArtifactFile {
                path: package_root.to_owned(),
                source,
            });
    }
    let mut cursor = package_root.parent().map(Path::to_path_buf);
    while let Some(root) = cursor {
        let candidate = root.join("Cargo.toml");
        if candidate.is_file() {
            let candidate_value = read_toml_file(&candidate)?;
            if candidate_value
                .get("workspace")
                .is_some_and(toml::Value::is_table)
            {
                return root
                    .canonicalize()
                    .map_err(|source| Error::ArtifactFile { path: root, source });
            }
        }
        cursor = root.parent().map(Path::to_path_buf);
    }
    package_root
        .canonicalize()
        .map_err(|source| Error::ArtifactFile {
            path: package_root.to_owned(),
            source,
        })
}

fn read_toml_file(path: &Path) -> Result<toml::Value, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| Error::ArtifactInvalid {
        path: path.to_owned(),
        message: format!("invalid captured Cargo manifest: {source}"),
    })
}

fn write_toml_file(path: &Path, value: &toml::Value) -> Result<(), Error> {
    let text = toml::to_string_pretty(value).map_err(|source| Error::ArtifactInvalid {
        path: path.to_owned(),
        message: format!("cannot serialize captured Cargo manifest: {source}"),
    })?;
    write_source_file(path, text.as_bytes())
}

fn copy_source_file(source: &Path, destination: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source).map_err(|error| Error::ArtifactFile {
        path: source.to_owned(),
        source: error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::ArtifactInvalid {
            path: source.to_owned(),
            message: "captured source input must be a regular file".to_owned(),
        });
    }
    let bytes = fs::read(source).map_err(|error| Error::ArtifactFile {
        path: source.to_owned(),
        source: error,
    })?;
    write_source_file(destination, &bytes)
}

fn write_source_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut file = File::create(path).map_err(|source| Error::BundleWrite {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::BundleWrite {
            path: path.to_owned(),
            source,
        })
}

fn copy_source_tree(source: &Path, destination: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source).map_err(|error| Error::ArtifactFile {
        path: source.to_owned(),
        source: error,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::ArtifactInvalid {
            path: source.to_owned(),
            message: "resolved Cargo source contains a symbolic link".to_owned(),
        });
    }
    if metadata.is_file() {
        return copy_source_file(source, destination);
    }
    if !metadata.is_dir() {
        return Err(Error::ArtifactInvalid {
            path: source.to_owned(),
            message: "resolved Cargo source is not a regular file or directory".to_owned(),
        });
    }
    fs::create_dir_all(destination).map_err(|error| Error::BundleDirectory {
        path: destination.to_owned(),
        source: error,
    })?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| Error::ArtifactFile {
            path: source.to_owned(),
            source: error,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| Error::ArtifactFile {
            path: source.to_owned(),
            source: error,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == ".git"
            || name == ".cargo-ok"
            || name == ".cargo_vcs_info.json"
            || name == "credentials"
            || name == "credentials.toml"
            || name == "target"
            || name == ".codex"
        {
            continue;
        }
        copy_source_tree(&entry.path(), &destination.join(name))?;
    }
    Ok(())
}

fn rewrite_workspace_paths(
    value: &mut toml::Value,
    workspace_root: &Path,
    staged_root: &Path,
    locations: &BTreeMap<PathBuf, PathBuf>,
) -> Result<(), Error> {
    let Some(workspace) = value
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
    else {
        return Ok(());
    };
    if let Some(dependencies) = workspace.get_mut("dependencies") {
        rewrite_dependency_table_paths(
            dependencies,
            workspace_root,
            workspace_root,
            staged_root,
            staged_root,
            locations,
        )?;
    }
    rewrite_override_paths(value, workspace_root, staged_root, locations)?;
    Ok(())
}

fn rewrite_local_paths(
    value: &mut toml::Value,
    package_root: &Path,
    workspace_root: &Path,
    locations: &BTreeMap<PathBuf, PathBuf>,
) -> Result<bool, Error> {
    let staged_package = locations
        .get(package_root)
        .ok_or_else(|| Error::ArtifactInvalid {
            path: package_root.to_owned(),
            message: "captured package has no staged location".to_owned(),
        })?
        .clone();
    let staged_workspace = locations
        .get(workspace_root)
        .cloned()
        .unwrap_or_else(|| staged_package.clone());
    let before = serde_json::to_vec(value).map_err(|error| Error::BundleJson {
        path: package_root.join("Cargo.toml"),
        source: error,
    })?;
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = value.get_mut(section) {
            rewrite_dependency_table_paths(
                dependencies,
                package_root,
                workspace_root,
                &staged_package,
                &staged_workspace,
                locations,
            )?;
        }
    }
    if let Some(targets) = value.get_mut("target").and_then(toml::Value::as_table_mut) {
        for target in targets.iter_mut().map(|(_, value)| value) {
            rewrite_local_paths(target, package_root, workspace_root, locations)?;
        }
    }
    rewrite_override_paths(value, package_root, &staged_package, locations)?;
    let after = serde_json::to_vec(value).map_err(|error| Error::BundleJson {
        path: package_root.join("Cargo.toml"),
        source: error,
    })?;
    Ok(before != after)
}

fn rewrite_dependency_table_paths(
    value: &mut toml::Value,
    package_root: &Path,
    workspace_root: &Path,
    staged_package: &Path,
    staged_workspace: &Path,
    locations: &BTreeMap<PathBuf, PathBuf>,
) -> Result<(), Error> {
    let Some(table) = value.as_table_mut() else {
        return Ok(());
    };
    let paths = table
        .iter()
        .filter_map(|(key, dependency)| {
            let dependency = dependency.as_table()?;
            let path = dependency.get("path")?.as_str()?;
            Some((
                key.clone(),
                path.to_owned(),
                dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true),
            ))
        })
        .collect::<Vec<_>>();
    for (key, path, uses_workspace) in paths {
        let (base, staged_base) = if uses_workspace {
            (workspace_root, staged_workspace)
        } else {
            (package_root, staged_package)
        };
        let canonical = resolve_local_dependency(base, &path)?;
        let Some(staged_dependency) = locations.get(&canonical) else {
            continue;
        };
        let staged_path = relative_path(staged_base, staged_dependency).ok_or_else(|| {
            Error::ArtifactInvalid {
                path: staged_dependency.clone(),
                message: "captured path dependency is outside the source closure".to_owned(),
            }
        })?;
        if let Some(dependency) = table.get_mut(&key).and_then(toml::Value::as_table_mut) {
            dependency.insert(
                "path".to_owned(),
                toml::Value::String(path_string(&staged_path)),
            );
        }
    }
    Ok(())
}

fn rewrite_override_paths(
    value: &mut toml::Value,
    package_root: &Path,
    staged_package: &Path,
    locations: &BTreeMap<PathBuf, PathBuf>,
) -> Result<(), Error> {
    for section in ["patch", "replace"] {
        let Some(overrides) = value.get_mut(section).and_then(toml::Value::as_table_mut) else {
            continue;
        };
        for table in overrides.iter_mut().map(|(_, value)| value) {
            rewrite_dependency_table_paths(
                table,
                package_root,
                package_root,
                staged_package,
                staged_package,
                locations,
            )?;
        }
    }
    Ok(())
}

fn resolve_local_dependency(base: &Path, reference: &str) -> Result<PathBuf, Error> {
    let path = Path::new(reference);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::RootDir | Component::Prefix(_)))
    {
        return Err(Error::ArtifactInvalid {
            path: base.join(path),
            message: "Cargo path dependency must be relative".to_owned(),
        });
    }
    base.join(path)
        .canonicalize()
        .map_err(|source| Error::ArtifactFile {
            path: base.join(path),
            source,
        })
}

fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &from[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to[common..] {
        if let Component::Normal(component) = component {
            relative.push(component);
        }
    }
    Some(relative)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn source_closure(prepared: &PreparedProject) -> Result<(Vec<BundleSource>, String), Error> {
    let package_ids = resolved_package_ids(prepared);
    let checksums = lock_checksums(&prepared.cargo_lock())?;
    let mut sources = prepared
        .metadata()
        .packages
        .iter()
        .filter(|package| package_ids.contains(&package.id.to_string()))
        .map(|package| source_record(prepared, package, &checksums))
        .collect::<Result<Vec<_>, _>>()?;
    sources.sort_by(|left, right| left.identity.cmp(&right.identity));

    let mut hasher = Sha256::new();
    for source in &sources {
        let serialized = serde_json::to_vec(source).map_err(|error| Error::BundleJson {
            path: PathBuf::from("provenance.json"),
            source: error,
        })?;
        update_digest_bytes(&mut hasher, &serialized);
    }
    Ok((sources, format!("{:x}", hasher.finalize())))
}

fn resolved_package_ids(prepared: &PreparedProject) -> BTreeSet<String> {
    let Some(resolve) = prepared.metadata().resolve.as_ref() else {
        return prepared
            .metadata()
            .packages
            .iter()
            .map(|package| package.id.to_string())
            .collect();
    };
    let Some(root) = resolve.root.as_ref() else {
        return prepared
            .metadata()
            .packages
            .iter()
            .map(|package| package.id.to_string())
            .collect();
    };
    let mut pending = VecDeque::from([root.to_string()]);
    let mut selected = BTreeSet::new();
    while let Some(id) = pending.pop_front() {
        if !selected.insert(id.clone()) {
            continue;
        }
        if let Some(node) = resolve.nodes.iter().find(|node| node.id.to_string() == id) {
            pending.extend(node.dependencies.iter().map(ToString::to_string));
        }
    }
    selected
}

fn source_record(
    prepared: &PreparedProject,
    package: &cargo_metadata::Package,
    checksums: &BTreeMap<(String, String, String), String>,
) -> Result<BundleSource, Error> {
    let raw_source = package
        .source
        .as_ref()
        .map_or_else(|| "local".to_owned(), |source| source.repr.clone());
    let kind = match package.source.as_ref().map(|source| source.repr.as_str()) {
        None => BundleSourceKind::Local,
        Some(source) if source.starts_with("git+") => BundleSourceKind::Git,
        Some(source) if source.starts_with("registry+") => BundleSourceKind::Registry,
        Some(source) => {
            return Err(Error::ArtifactInvalid {
                path: PathBuf::from(package.manifest_path.as_std_path()),
                message: format!("unsupported Cargo source scheme '{source}'"),
            });
        }
    };
    let source = if kind == BundleSourceKind::Git {
        sanitize_git_source(
            &raw_source,
            &PathBuf::from(package.manifest_path.as_std_path()),
        )?
    } else {
        raw_source
    };
    let package_root = PathBuf::from(package.manifest_path.as_std_path())
        .parent()
        .ok_or_else(|| Error::ArtifactInvalid {
            path: PathBuf::from(package.manifest_path.as_std_path()),
            message: "Cargo package manifest has no parent directory".to_owned(),
        })?
        .to_owned();
    let files = source_files_with(&package_root, kind == BundleSourceKind::Registry)?;
    let digest = digest_source_files(&files);
    let package_id = public_package_id(prepared, &package.id.to_string());
    let identity = format!("{package_id}#{digest}");
    let git = if kind == BundleSourceKind::Git {
        let source = package
            .source
            .as_ref()
            .map(|source| source.repr.as_str())
            .ok_or_else(|| Error::ArtifactInvalid {
                path: package_root.join("Cargo.toml"),
                message: "Git source classification has no source identity".to_owned(),
            })?;
        Some(git_source(source, &package_root)?)
    } else {
        None
    };
    let registry_checksum = if kind == BundleSourceKind::Registry {
        Some(
            checksums
                .get(&(
                    package.name.to_string(),
                    package.version.to_string(),
                    source.clone(),
                ))
                .cloned()
                .ok_or_else(|| Error::ArtifactInvalid {
                    path: package_root.join("Cargo.toml"),
                    message: format!(
                        "Cargo.lock has no checksum for registry package {} {} from {}",
                        package.name, package.version, source
                    ),
                })?,
        )
    } else {
        None
    };
    Ok(BundleSource {
        identity,
        package_id,
        package: package.name.to_string(),
        version: package.version.to_string(),
        source,
        kind,
        digest,
        files,
        registry_checksum,
        git,
    })
}

fn source_files(root: &Path) -> Result<Vec<BundleSourceFile>, Error> {
    source_files_with(root, false)
}

fn source_files_with(
    root: &Path,
    preserve_registry_metadata: bool,
) -> Result<Vec<BundleSourceFile>, Error> {
    let mut pending = VecDeque::from([PathBuf::new()]);
    let mut paths = Vec::new();
    while let Some(relative) = pending.pop_front() {
        let directory = root.join(&relative);
        let mut entries = fs::read_dir(&directory)
            .map_err(|source| Error::ArtifactFile {
                path: directory.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| Error::ArtifactFile {
                path: directory.clone(),
                source,
            })?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            if name == ".git"
                || name == ".cargo-ok"
                || (name == ".cargo_vcs_info.json" && !preserve_registry_metadata)
                || name == "credentials"
                || name == "credentials.toml"
                || name == "target"
                || name == ".codex"
            {
                continue;
            }
            let child = relative.join(name);
            let path = root.join(&child);
            let metadata = fs::symlink_metadata(&path).map_err(|source| Error::ArtifactFile {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() {
                return Err(Error::ArtifactInvalid {
                    path,
                    message: "resolved Cargo source contains a symbolic link".to_owned(),
                });
            }
            if metadata.is_dir() {
                pending.push_back(child);
            } else if metadata.is_file() {
                paths.push(child);
            } else {
                return Err(Error::ArtifactInvalid {
                    path,
                    message: "resolved Cargo source contains a non-regular entry".to_owned(),
                });
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|relative| {
            let path = root.join(&relative);
            let digest = digest_file(&path)?;
            Ok(BundleSourceFile {
                path: relative.to_string_lossy().replace('\\', "/"),
                sha256: digest.sha256,
                bytes: digest.bytes,
            })
        })
        .collect()
}

fn digest_source_tree(root: &Path) -> Result<String, Error> {
    Ok(digest_source_files(&source_files(root)?))
}

fn validate_cargo_config(root: &Path) -> Result<(), Error> {
    let files = source_files(root)?;
    for file in files {
        if !file.path.ends_with(".toml") {
            continue;
        }
        let path = root.join(&file.path);
        let text = fs::read_to_string(&path).map_err(|source| Error::ArtifactFile {
            path: path.clone(),
            source,
        })?;
        if text.lines().any(|line| {
            let line = line.trim_start().to_ascii_lowercase();
            line.starts_with("token") || line.starts_with("password") || line.starts_with("secret")
        }) {
            return Err(Error::ArtifactInvalid {
                path,
                message: "Cargo configuration contains credential material".to_owned(),
            });
        }
    }
    Ok(())
}

fn digest_source_files(files: &[BundleSourceFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        update_digest_bytes(&mut hasher, file.path.as_bytes());
        update_digest_bytes(&mut hasher, file.sha256.as_bytes());
        update_digest_bytes(&mut hasher, &file.bytes.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn lock_checksums(path: &Path) -> Result<BTreeMap<(String, String, String), String>, Error> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(BTreeMap::new());
    };
    let text = String::from_utf8(bytes).map_err(|error| Error::ArtifactInvalid {
        path: path.to_owned(),
        message: format!("Cargo.lock is not UTF-8: {error}"),
    })?;
    let value = toml::from_str::<toml::Value>(&text).map_err(|error| Error::ArtifactInvalid {
        path: path.to_owned(),
        message: format!("Cargo.lock is not valid TOML: {error}"),
    })?;
    let mut checksums = BTreeMap::new();
    if let Some(packages) = value.get("package").and_then(toml::Value::as_array) {
        for package in packages {
            let Some(table) = package.as_table() else {
                continue;
            };
            let Some(checksum) = table.get("checksum").and_then(toml::Value::as_str) else {
                continue;
            };
            let (Some(name), Some(version), Some(source)) = (
                table.get("name").and_then(toml::Value::as_str),
                table.get("version").and_then(toml::Value::as_str),
                table.get("source").and_then(toml::Value::as_str),
            ) else {
                continue;
            };
            checksums.insert(
                (name.to_owned(), version.to_owned(), source.to_owned()),
                checksum.to_owned(),
            );
        }
    }
    Ok(checksums)
}

fn git_source(source: &str, package_root: &Path) -> Result<BundleGitSource, Error> {
    let value = source
        .strip_prefix("git+")
        .and_then(|value| value.rsplit_once('#'))
        .ok_or_else(|| Error::ArtifactInvalid {
            path: package_root.join("Cargo.toml"),
            message: "Git package source is missing its resolved immutable revision".to_owned(),
        })?;
    let repository_with_query = value.0;
    let revision = value.1;
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::ArtifactInvalid {
            path: package_root.join("Cargo.toml"),
            message: format!("Git package source revision '{revision}' is not a full commit"),
        });
    }
    let repository = repository_with_query
        .split_once('?')
        .map_or(repository_with_query, |(repository, _)| repository)
        .to_owned();
    let repository = safe_git_repository(&repository, package_root)?;
    let git_root =
        git_output(package_root, &["rev-parse", "--show-toplevel"]).ok_or_else(|| {
            Error::ArtifactInvalid {
                path: package_root.to_owned(),
                message: "Cargo Git source is not inside a readable Git checkout".to_owned(),
            }
        })?;
    let git_root =
        PathBuf::from(git_root)
            .canonicalize()
            .map_err(|source| Error::ArtifactFile {
                path: package_root.to_owned(),
                source,
            })?;
    let package_root = package_root
        .canonicalize()
        .map_err(|source| Error::ArtifactFile {
            path: package_root.to_owned(),
            source,
        })?;
    let subdirectory = package_root
        .strip_prefix(&git_root)
        .map(path_string)
        .map_err(|_| Error::ArtifactInvalid {
            path: package_root.clone(),
            message: format!(
                "Cargo Git source checkout {} does not contain package root {}",
                git_root.display(),
                package_root.display()
            ),
        })?;
    let head = git_output(&package_root, &["rev-parse", "HEAD"]).ok_or_else(|| {
        Error::ArtifactInvalid {
            path: package_root.clone(),
            message: "Cargo Git source checkout has no readable HEAD".to_owned(),
        }
    })?;
    if !head.eq_ignore_ascii_case(revision) {
        return Err(Error::ArtifactInvalid {
            path: package_root,
            message: format!(
                "Cargo Git source HEAD {head} does not match resolved revision {revision}"
            ),
        });
    }
    let dirty = git_status(&package_root)?;
    if !dirty.is_empty() {
        return Err(Error::ArtifactInvalid {
            path: package_root,
            message: format!(
                "Cargo Git source checkout has source changes outside Cargo metadata: {}",
                dirty.join(", ")
            ),
        });
    }
    Ok(BundleGitSource {
        repository,
        revision: revision.to_owned(),
        subdirectory,
    })
}

fn sanitize_git_source(source: &str, path: &Path) -> Result<String, Error> {
    let value = source
        .strip_prefix("git+")
        .ok_or_else(|| Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "Git package source is missing Cargo's git source prefix".to_owned(),
        })?;
    let (repository, revision) = value
        .rsplit_once('#')
        .ok_or_else(|| Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "Git package source is missing its resolved immutable revision".to_owned(),
        })?;
    let repository = repository
        .split_once('?')
        .map_or(repository, |(repository, _)| repository);
    let repository = safe_git_repository(repository, path)?;
    Ok(format!("git+{repository}#{revision}"))
}

fn safe_git_repository(repository: &str, path: &Path) -> Result<String, Error> {
    if let Some(authority) = repository.split_once("://").map(|(_, rest)| rest)
        && authority
            .split_once('/')
            .map_or(authority, |(authority, _)| authority)
            .contains('@')
    {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "Git source URL contains credentials".to_owned(),
        });
    }
    if repository.starts_with("file:") || repository.starts_with('/') {
        return Ok("local-git".to_owned());
    }
    Ok(repository.to_owned())
}

fn git_output(package_root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(package_root)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn git_status(package_root: &Path) -> Result<Vec<String>, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(package_root)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .map_err(|source| Error::ArtifactFile {
            path: package_root.to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::ArtifactInvalid {
            path: package_root.to_owned(),
            message: format!(
                "cannot inspect Cargo Git source status: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim();
            let path = path.rsplit_once(" -> ").map_or(path, |(_, path)| path);
            let file_name = Path::new(path).file_name().and_then(|name| name.to_str());
            (!matches!(file_name, Some(".cargo-ok" | ".cargo_vcs_info.json")))
                .then_some(line.to_owned())
        })
        .collect())
}

fn public_package_id(prepared: &PreparedProject, package_id: &str) -> String {
    if let Some(package) = prepared
        .metadata()
        .packages
        .iter()
        .find(|package| package.id.to_string() == package_id)
    {
        if package.source.is_none() {
            return format!("local:{}@{}", package.name, package.version);
        }
        if let Some(source) = package.source.as_ref()
            && source.repr.starts_with("git+")
        {
            let revision = source
                .repr
                .rsplit_once('#')
                .map_or("unknown", |(_, revision)| revision);
            return format!("git:{}@{}#{revision}", package.name, package.version);
        }
        return package.id.to_string();
    }
    if package_id.starts_with("path+") {
        "local".to_owned()
    } else {
        package_id.to_owned()
    }
}

fn stage_model(
    prepared: &PreparedProject,
    staged_root: &Path,
) -> Result<Option<StagedModel>, Error> {
    let Some(path) = prepared.document().robot.model.as_ref() else {
        return Ok(None);
    };
    let relative = safe_input_path(path)?;
    let root = prepared.layout().root();
    let full = safe_source_file(root, &relative)?;
    let source_digest = digest_file(&full)?;
    let source = BundleFile {
        path: relative.to_string_lossy().replace('\\', "/"),
        sha256: source_digest.sha256,
        bytes: source_digest.bytes,
    };
    let closure = closed_robot_model(root, &relative, &full)?;
    let mut resources = Vec::new();
    for resource in &closure.resources {
        let relative_path = format!("{ASSET_DIR}/{}", resource.name);
        let destination = staged_root.join(&relative_path);
        write_model_resource(&destination, resource)?;
        let digest = digest_bytes(&resource.bytes);
        resources.push(BundleResource {
            path: relative_path,
            sha256: digest.sha256,
            bytes: digest.bytes,
        });
    }
    Ok(Some(StagedModel {
        source,
        closure: BundleModelClosure {
            entry: format!("{ASSET_DIR}/{}", closure.entry),
            digest: closure.digest,
            resources,
        },
    }))
}

fn closed_robot_model(root: &Path, relative: &Path, full: &Path) -> Result<ModelClosure, Error> {
    let parent = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        let resource_root = root.join(parent);
        let entry = relative
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                invalid_model(root, relative, "model path must have a UTF-8 file name")
            })?;
        let resources = collect_model_files(&resource_root, &resource_root, root, relative)?;
        return model_closure(entry, resources)
            .map_err(|message| invalid_model(root, relative, message));
    }

    let entry = relative
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_model(root, relative, "model path must have a UTF-8 file name"))?;
    let mut resources = vec![StagedResource {
        name: entry.to_owned(),
        bytes: read_model_bytes(full, root, relative)?,
    }];
    let assets = root.join(ASSET_DIR);
    if assets.exists() {
        let metadata = fs::symlink_metadata(&assets).map_err(|source| Error::ArtifactFile {
            path: assets.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid_model(
                root,
                relative,
                "model assets must be a regular directory",
            ));
        }
        resources.extend(collect_model_files(root, &assets, root, relative)?);
    }
    model_closure(entry, resources).map_err(|message| invalid_model(root, relative, message))
}

fn collect_model_files(
    resource_root: &Path,
    directory: &Path,
    root: &Path,
    model_relative: &Path,
) -> Result<Vec<StagedResource>, Error> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| Error::ArtifactFile {
            path: directory.to_owned(),
            source,
        })?
        .map(|entry| {
            entry
                .map_err(|source| Error::ArtifactFile {
                    path: directory.to_owned(),
                    source,
                })
                .and_then(|entry| {
                    let path = entry.path();
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|source| Error::ArtifactFile {
                            path: path.clone(),
                            source,
                        })?;
                    let relative =
                        path.strip_prefix(resource_root)
                            .map_err(|_| Error::ArtifactInvalid {
                                path: path.clone(),
                                message: "model resource escaped its resource root".to_owned(),
                            })?;
                    Ok((relative.to_owned(), path, metadata))
                })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut resources = Vec::new();
    for (relative, path, metadata) in entries {
        if metadata.file_type().is_symlink() {
            return Err(invalid_model(
                root,
                model_relative,
                "model resources must not contain symlinks",
            ));
        }
        if metadata.is_dir() {
            resources.extend(collect_model_files(
                resource_root,
                &path,
                root,
                model_relative,
            )?);
        } else if metadata.is_file() {
            let name = normalize_model_resource_name(&relative)
                .map_err(|message| invalid_model(root, model_relative, message))?;
            let bytes = read_model_bytes(&path, root, model_relative)?;
            resources.push(StagedResource { name, bytes });
        } else {
            return Err(invalid_model(
                root,
                model_relative,
                "model resources must be regular files or directories",
            ));
        }
    }
    Ok(resources)
}

fn read_model_bytes(path: &Path, root: &Path, relative: &Path) -> Result<Vec<u8>, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_model(
            root,
            relative,
            "model resources must be regular files",
        ));
    }
    fs::read(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })
}

fn write_model_resource(path: &Path, resource: &StagedResource) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut file = File::create(path).map_err(|source| Error::BundleWrite {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&resource.bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::BundleWrite {
            path: path.to_owned(),
            source,
        })
}

fn model_closure(entry: &str, mut resources: Vec<StagedResource>) -> Result<ModelClosure, String> {
    if entry.is_empty() {
        return Err("model entry name must not be empty".to_owned());
    }
    normalize_model_resource_name(Path::new(entry))?;
    resources.sort_by(|left, right| left.name.cmp(&right.name));
    for pair in resources.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!(
                "model resource {:?} appears more than once",
                pair[0].name
            ));
        }
    }
    if !resources.iter().any(|resource| resource.name == entry) {
        return Err(format!(
            "model entry {entry:?} is not present in the resource closure"
        ));
    }
    Ok(ModelClosure {
        digest: digest_model_closure(entry, &resources),
        entry: entry.to_owned(),
        resources,
    })
}

fn normalize_model_resource_name(path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.to_string_lossy().contains('\\')
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "model resource name {:?} must be relative, normalized, and use '/' separators",
            path
        ));
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn digest_model_closure(entry: &str, resources: &[StagedResource]) -> String {
    let mut hasher = Sha256::new();
    update_digest_bytes(&mut hasher, entry.as_bytes());
    for resource in resources {
        update_digest_bytes(&mut hasher, resource.name.as_bytes());
        update_digest_bytes(&mut hasher, &resource.bytes);
    }
    format!("{:x}", hasher.finalize())
}

fn update_digest_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[derive(Debug, Clone)]
struct ModelClosure {
    entry: String,
    resources: Vec<StagedResource>,
    digest: String,
}

fn invalid_model(root: &Path, relative: &Path, message: impl Into<String>) -> Error {
    Error::ArtifactInvalid {
        path: root.join(relative),
        message: message.into(),
    }
}

fn optional_file(path: &Path, relative: &str) -> Result<Option<BundleFile>, Error> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            let digest = digest_file(path)?;
            Ok(Some(BundleFile {
                path: relative.to_owned(),
                sha256: digest.sha256,
                bytes: digest.bytes,
            }))
        }
        Ok(_) => Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "authored Cargo.lock path is not a regular file".to_owned(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::ArtifactFile {
            path: path.to_owned(),
            source,
        }),
    }
}

#[derive(Debug, Clone)]
struct FileDigest {
    sha256: String,
    bytes: u64,
}

fn digest_file(path: &Path) -> Result<FileDigest, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "bundle input must be a regular file, not a link".to_owned(),
        });
    }
    let mut file = File::open(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| Error::ArtifactFile {
                path: path.to_owned(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| Error::ArtifactInvalid {
                path: path.to_owned(),
                message: "file size exceeds the supported bundle counter".to_owned(),
            })?;
    }
    Ok(FileDigest {
        sha256: format!("{:x}", hasher.finalize()),
        bytes,
    })
}

fn ensure_regular_executable(path: &Path, target: &str) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: format!("Cargo reported a non-regular {target} executable"),
        });
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> FileDigest {
    FileDigest {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        bytes: bytes.len() as u64,
    }
}

fn copy_executable(source: &Path, destination: &Path) -> Result<(), Error> {
    fs::copy(source, destination).map_err(|source_error| Error::BundleCopy {
        from: source.to_owned(),
        to: destination.to_owned(),
        source: source_error,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(destination)
            .map_err(|source| Error::ArtifactFile {
                path: destination.to_owned(),
                source,
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(destination, permissions).map_err(|source| Error::ArtifactFile {
            path: destination.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| Error::BundleJson {
        path: path.to_owned(),
        source,
    })?;
    let mut file = File::create(path).map_err(|source| Error::BundleWrite {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::BundleWrite {
            path: path.to_owned(),
            source,
        })
}

fn publish_directory(staged: &Path, output: &Path) -> Result<(), Error> {
    if output.exists() {
        if !output.is_dir() {
            return Err(Error::BundlePublish {
                path: output.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "bundle output exists and is not a directory",
                ),
            });
        }
        if output.is_dir() && directories_equal(staged, output)? {
            fs::remove_dir_all(staged).map_err(|source| Error::BundleCleanup {
                path: staged.to_owned(),
                source,
            })?;
            return Ok(());
        }
        let old_parent = tempfile::Builder::new()
            .prefix(".phoxal-previous-")
            .tempdir_in(output.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|source| Error::BundleDirectory {
                path: output.to_owned(),
                source,
            })?;
        let old = old_parent.path().join("bundle");
        fs::rename(output, &old).map_err(|source| Error::BundlePublish {
            path: output.to_owned(),
            source,
        })?;
        if let Err(source) = fs::rename(staged, output) {
            let _ = fs::rename(&old, output);
            return Err(Error::BundlePublish {
                path: output.to_owned(),
                source,
            });
        }
        fs::remove_dir_all(&old).map_err(|source| Error::BundleCleanup { path: old, source })?;
        return Ok(());
    }
    fs::rename(staged, output).map_err(|source| Error::BundlePublish {
        path: output.to_owned(),
        source,
    })
}

struct BundlePublicationLock {
    file: File,
    key: PathBuf,
}

static ACTIVE_BUNDLE_PUBLICATION_LOCKS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

impl Drop for BundlePublicationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        if let Some(active) = ACTIVE_BUNDLE_PUBLICATION_LOCKS.get()
            && let Ok(mut active) = active.lock()
        {
            active.remove(&self.key);
        }
    }
}

fn acquire_bundle_publication_lock(output: &Path) -> Result<BundlePublicationLock, Error> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = parent
        .canonicalize()
        .map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    let output_name = output.file_name().ok_or_else(|| Error::ArtifactInvalid {
        path: output.to_owned(),
        message: "bundle output must name a directory below an existing parent".to_owned(),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(canonical_parent.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(output_name.as_encoded_bytes());
    let lock_directory = canonical_parent.join(".phoxal-bundle-locks");
    fs::create_dir_all(&lock_directory).map_err(|source| Error::BundleDirectory {
        path: lock_directory.clone(),
        source,
    })?;
    let lock_path = lock_directory.join(format!("{:x}.lock", hasher.finalize()));
    let active = ACTIVE_BUNDLE_PUBLICATION_LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()));
    let mut active = active.lock().map_err(|_| Error::BundleLock {
        path: lock_path.clone(),
        source: io::Error::other("bundle publication lock registry is poisoned"),
    })?;
    if active.contains(&lock_path) {
        return Err(Error::BundleBusy {
            path: output.to_owned(),
        });
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| Error::BundleLock {
            path: lock_path.clone(),
            source,
        })?;
    match FileExt::try_lock(&lock) {
        Ok(()) => {
            active.insert(lock_path.clone());
            Ok(BundlePublicationLock {
                file: lock,
                key: lock_path,
            })
        }
        Err(TryLockError::WouldBlock) => Err(Error::BundleBusy {
            path: output.to_owned(),
        }),
        Err(TryLockError::Error(source)) => Err(Error::BundleLock {
            path: lock_path,
            source,
        }),
    }
}

fn directories_equal(left: &Path, right: &Path) -> Result<bool, Error> {
    let left_entries = directory_entries(left)?;
    let right_entries = directory_entries(right)?;
    if left_entries != right_entries {
        return Ok(false);
    }
    for relative in left_entries {
        let left_path = left.join(&relative);
        let right_path = right.join(&relative);
        let left_metadata =
            fs::symlink_metadata(&left_path).map_err(|source| Error::ArtifactFile {
                path: left_path.clone(),
                source,
            })?;
        let right_metadata =
            fs::symlink_metadata(&right_path).map_err(|source| Error::ArtifactFile {
                path: right_path.clone(),
                source,
            })?;
        if left_metadata.is_dir() != right_metadata.is_dir() {
            return Ok(false);
        }
        if left_metadata.is_file()
            && fs::read(&left_path).map_err(|source| Error::ArtifactFile {
                path: left_path.clone(),
                source,
            })? != fs::read(&right_path).map_err(|source| Error::ArtifactFile {
                path: right_path.clone(),
                source,
            })?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn directory_entries(root: &Path) -> Result<BTreeSet<PathBuf>, Error> {
    let mut pending = vec![PathBuf::new()];
    let mut entries = BTreeSet::new();
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        for entry in fs::read_dir(&directory).map_err(|source| Error::BundleDirectory {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::BundleDirectory {
                path: directory.clone(),
                source,
            })?;
            let child = relative.join(entry.file_name());
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| Error::ArtifactFile {
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
                return Err(Error::ArtifactInvalid {
                    path: entry.path(),
                    message: "bundle contains an unsupported filesystem entry".to_owned(),
                });
            }
            entries.insert(child.clone());
            if metadata.is_dir() {
                pending.push(child);
            }
        }
    }
    Ok(entries)
}

fn safe_bundle_name(value: &str) -> Result<String, Error> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_uppercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_')
        })
    {
        return Err(Error::ArtifactInvalid {
            path: PathBuf::from(value),
            message: "executable instance is not a safe bundle filename".to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn safe_input_path(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "authored input path must remain below the robot root".to_owned(),
        });
    }
    Ok(path.to_owned())
}

fn safe_source_file(root: &Path, relative: &Path) -> Result<PathBuf, Error> {
    let root = root.canonicalize().map_err(|source| Error::ArtifactFile {
        path: root.to_owned(),
        source,
    })?;
    let full = root.join(relative);
    let mut current = root.clone();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(Error::ArtifactInvalid {
                path: full,
                message: "authored input path must be normalized".to_owned(),
            });
        };
        current.push(part);
        let metadata = fs::symlink_metadata(&current).map_err(|source| Error::ArtifactFile {
            path: current.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(Error::ArtifactInvalid {
                path: current,
                message: "authored model path must not contain symlinks".to_owned(),
            });
        }
    }
    let canonical = full.canonicalize().map_err(|source| Error::ArtifactFile {
        path: full.clone(),
        source,
    })?;
    if !canonical.starts_with(&root) {
        return Err(Error::ArtifactInvalid {
            path: full,
            message: "authored input resolves outside the robot root".to_owned(),
        });
    }
    Ok(canonical)
}

fn validate_identity_part(field: &'static str, value: &str) -> Result<(), Error> {
    let valid = (1..=64).contains(&value.len())
        && value.is_ascii()
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().skip(1).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidExecutionIdentity {
            field,
            value: value.to_owned(),
        })
    }
}

fn source_identity(source: &PackageSource) -> Result<String, Error> {
    match source {
        PackageSource::Local { .. } => Ok("local".to_owned()),
        PackageSource::Git { source } => sanitize_git_source(source, Path::new("Cargo.toml")),
        PackageSource::Registry { source } | PackageSource::Other { source } => Ok(source.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_identity_uses_bounded_key_segments() {
        let identity = LocalIdentity::new("local", "local").expect("local identity is valid");
        assert_eq!(identity.scope(), "local");
        assert!(LocalIdentity::new("../outside", "local").is_err());
    }

    #[test]
    fn bundle_names_reject_path_traversal_and_empty_values() {
        assert!(safe_bundle_name("../outside").is_err());
        assert!(safe_bundle_name("").is_err());
        assert_eq!(
            safe_bundle_name("front_service").expect("valid name"),
            "front_service"
        );
    }

    #[test]
    fn source_identity_does_not_leak_local_absolute_paths() {
        assert_eq!(
            source_identity(&PackageSource::Local {
                manifest_path: PathBuf::from("/private/checkout/Cargo.toml"),
            })
            .expect("local identity"),
            "local"
        );
    }

    #[test]
    fn effective_bundle_inputs_include_passthrough_cargo_flags() {
        let options = CargoOptions {
            cargo_args: vec![
                "--target".into(),
                "aarch64-unknown-linux-gnu".into(),
                "--target=x86_64-unknown-linux-gnu".into(),
                "--profile".into(),
                "custom".into(),
                "--release".into(),
                "--features".into(),
                "camera,imu".into(),
                "telemetry".into(),
                "--features=vision localization".into(),
                "--all-features".into(),
            ],
            ..CargoOptions::default()
        };
        assert_eq!(effective_target(&options), "x86_64-unknown-linux-gnu");
        assert_eq!(effective_profile(&options), "release");
        assert_eq!(
            effective_features(&options),
            ["camera", "imu", "localization", "telemetry", "vision"]
        );
        assert!(effective_all_features(&options));
    }

    #[test]
    fn build_environment_is_allowlisted_and_never_serializes_paths_or_secrets() {
        let values = select_build_environment([
            ("HOME".to_owned(), "/private/user".to_owned()),
            ("AWS_SECRET_ACCESS_KEY".to_owned(), "aws-secret".to_owned()),
            ("API_KEY".to_owned(), "api-secret".to_owned()),
            (
                "CARGO_REGISTRIES_PHOO_INDEX".to_owned(),
                "https://user:secret@example.invalid/index".to_owned(),
            ),
            (
                "RUSTFLAGS".to_owned(),
                "-C link-arg=/private/toolchain/libnative.a".to_owned(),
            ),
            (
                "CARGO_BUILD_TARGET".to_owned(),
                "aarch64-unknown-linux-gnu".to_owned(),
            ),
        ]);
        let mut names = values
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "CARGO_BUILD_TARGET",
                "CARGO_REGISTRIES_PHOO_INDEX",
                "RUSTFLAGS"
            ]
        );
        let rendered = render_build_environment(values);
        let serialized = serde_json::to_string(&rendered).expect("environment JSON");
        assert!(!serialized.contains("/private"));
        assert!(!serialized.contains("aws-secret"));
        assert!(!serialized.contains("api-secret"));
        assert!(!serialized.contains("user:secret"));
        assert!(serialized.contains("CARGO_BUILD_TARGET"));
    }

    #[test]
    fn provenance_text_normalizes_embedded_local_paths_and_git_credentials() {
        let value = normalize_provenance_text(
            "--target-dir=/private/build https://user:secret@example.invalid/repo /private/tool",
            Path::new("/private/project"),
        );
        assert!(!value.contains("/private"));
        assert!(!value.contains("user:secret"));
        assert!(value.contains("<local-path>"));
        assert!(value.contains("<redacted>"));
    }

    #[test]
    fn git_source_identity_redacts_local_paths_and_rejects_credentials() {
        let path = Path::new("Cargo.toml");
        assert_eq!(
            safe_git_repository("file:///private/checkout", path).expect("local URL"),
            "local-git"
        );
        assert!(matches!(
            safe_git_repository("https://user:secret@example.invalid/repo", path),
            Err(Error::ArtifactInvalid { message, .. }) if message.contains("credentials")
        ));
    }

    #[test]
    fn path_rewrites_preserve_target_specific_aliases_and_overrides()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let package = root.path().join("package");
        let host = root.path().join("host");
        let target = root.path().join("target");
        let patch = root.path().join("patch");
        for path in [&package, &host, &target, &patch] {
            fs::create_dir_all(path)?;
            fs::write(path.join("Cargo.toml"), "[package]\nname = \"fixture\"\n")?;
        }
        let staged_package = root.path().join("staged/package");
        let mut locations = BTreeMap::new();
        locations.insert(package.canonicalize()?, staged_package.clone());
        locations.insert(host.canonicalize()?, root.path().join("staged/deps/host"));
        locations.insert(
            target.canonicalize()?,
            root.path().join("staged/deps/target"),
        );
        locations.insert(patch.canonicalize()?, root.path().join("staged/deps/patch"));
        let mut value = toml::from_str(
            "[dependencies]\nfoo = { path = \"../host\" }\n\n[target.'cfg(unix)'.dependencies]\nfoo = { path = \"../target\" }\n\n[patch.crates-io]\nbar = { path = \"../patch\" }\n",
        )?;
        let package = package.canonicalize()?;
        assert!(rewrite_local_paths(
            &mut value, &package, &package, &locations
        )?);
        assert_eq!(
            value["dependencies"]["foo"]["path"].as_str(),
            Some("../deps/host")
        );
        assert_eq!(
            value["target"]["cfg(unix)"]["dependencies"]["foo"]["path"].as_str(),
            Some("../deps/target")
        );
        assert_eq!(
            value["patch"]["crates-io"]["bar"]["path"].as_str(),
            Some("../deps/patch")
        );
        Ok(())
    }

    #[test]
    fn source_inventory_excludes_cargo_cache_markers_except_registry_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = \"source\"\n",
        )?;
        fs::write(directory.path().join(".cargo-ok"), "")?;
        fs::write(directory.path().join(".cargo_vcs_info.json"), "{}")?;
        let local = source_files(directory.path())?;
        assert!(!local.iter().any(|file| file.path == ".cargo-ok"));
        assert!(!local.iter().any(|file| file.path == ".cargo_vcs_info.json"));
        let registry = source_files_with(directory.path(), true)?;
        assert!(!registry.iter().any(|file| file.path == ".cargo-ok"));
        assert!(
            registry
                .iter()
                .any(|file| file.path == ".cargo_vcs_info.json")
        );
        Ok(())
    }

    #[test]
    fn git_provenance_uses_checkout_identity_for_subdirectories()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        let package_root = repository.path().join("checkouts/service");
        fs::create_dir_all(package_root.join("src"))?;
        fs::write(
            package_root.join("Cargo.toml"),
            "[package]\nname = \"git-service\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        fs::write(package_root.join("src/lib.rs"), "pub struct Service;\n")?;
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repository.path())
            .output()?;
        assert!(init.status.success());
        for arguments in [
            vec!["config", "user.name", "Phoxal Test"],
            vec!["config", "user.email", "phoxal@example.invalid"],
            vec!["add", "."],
            vec!["commit", "--quiet", "-m", "fixture"],
        ] {
            let output = Command::new("git")
                .args(arguments)
                .current_dir(repository.path())
                .output()?;
            assert!(
                output.status.success(),
                "git command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let revision = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repository.path())
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        let source = format!("git+file://{}#{revision}", repository.path().display());
        let provenance = git_source(&source, &package_root)?;
        assert_eq!(provenance.revision, revision);
        assert_eq!(provenance.subdirectory, "checkouts/service");
        fs::write(package_root.join("src/lib.rs"), "pub struct Changed;\n")?;
        let error = git_source(&source, &package_root)
            .expect_err("a dirty Git source must not claim immutable provenance");
        assert!(matches!(
            error,
            Error::ArtifactInvalid { message, .. }
                if message.contains("source changes outside Cargo metadata")
        ));
        Ok(())
    }

    #[test]
    fn authored_input_paths_cannot_escape_the_robot_root() {
        assert!(safe_input_path(Path::new("../model.xml")).is_err());
        assert!(safe_input_path(Path::new("/tmp/model.xml")).is_err());
        assert_eq!(
            safe_input_path(Path::new("models/robot.xml")).expect("relative path"),
            PathBuf::from("models/robot.xml")
        );
    }

    #[test]
    fn bundle_publication_is_serialized_per_output() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let output = directory.path().join("bundle");
        let held = acquire_bundle_publication_lock(&output).expect("first publication lock");
        assert!(matches!(
            acquire_bundle_publication_lock(&output),
            Err(Error::BundleBusy { path }) if path == output
        ));
        drop(held);
        acquire_bundle_publication_lock(&output).expect("released publication lock");
    }
}
