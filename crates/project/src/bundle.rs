//! Deterministic compiled bundle assembly for one prepared robot project.
//!
//! The project compiler owns the source-side assembly boundary. It builds the
//! exact executable targets selected by robot.yaml, records the resolved Cargo
//! identities and authored inputs, and publishes one complete directory
//! atomically. This module deliberately does not launch a process, install a
//! release, or claim that a built executable is Ready.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use cargo_metadata::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cargo;
use crate::selection::{PackageSource, SelectedTarget};
use crate::{CargoOptions, Error, PreparedProject, RobotDocument};

/// The compiled project-bundle schema emitted by this source compiler.
pub const BUNDLE_SCHEMA: &str = "phoxal/bundle/v0";
const BIN_DIR: &str = "bin";
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
    /// brain or service.
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

/// Source and tool inputs used to construct a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleProvenance {
    /// Format discriminator for the provenance record.
    pub schema: String,
    /// SHA-256 of the authored robot document.
    pub robot_manifest_sha256: String,
    /// SHA-256 of the root Cargo manifest.
    pub cargo_manifest_sha256: String,
    /// SHA-256 of the workspace-owned Cargo lock, when present.
    pub cargo_lock_sha256: Option<String>,
    /// Model path and digest when the authored model exists.
    pub model: Option<BundleFile>,
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

pub(crate) fn assemble(
    prepared: &PreparedProject,
    options: &CargoOptions,
    output: impl AsRef<Path>,
) -> Result<CompiledBundle, Error> {
    options.validate()?;
    let output = output.as_ref();
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| Error::BundleDirectory {
        path: parent.to_owned(),
        source,
    })?;

    let staging = tempfile::Builder::new()
        .prefix(".phoxal-bundle-")
        .tempdir_in(parent)
        .map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    let staged_root = staging.path();
    fs::create_dir(staged_root.join(BIN_DIR)).map_err(|source| Error::BundleDirectory {
        path: staged_root.join(BIN_DIR),
        source,
    })?;

    let mut artifacts = BTreeMap::new();
    for (_, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        if artifacts.contains_key(&key) {
            continue;
        }
        let output = cargo::build_target(prepared, target, options)?;
        let executable = cargo_artifact(&output.stdout, target)?;
        let metadata = fs::symlink_metadata(&executable).map_err(|source| Error::ArtifactFile {
            path: executable.clone(),
            source,
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::ArtifactInvalid {
                path: executable,
                message: "Cargo reported a non-regular executable".to_owned(),
            });
        }
        let digest = digest_file(&executable)?;
        artifacts.insert(key, (target.clone(), executable, digest));
    }

    let mut executable_records = Vec::new();
    for (instance, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        let (built_target, source, digest) =
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
            role: if instance == "brain" {
                "brain".to_owned()
            } else {
                "service".to_owned()
            },
            instance: instance.clone(),
            package_id: built_target.package_id.clone(),
            package: built_target.package.clone(),
            target: built_target.target.clone(),
            path: relative,
            bytes: digest.bytes,
            sha256: digest.sha256.clone(),
        });
    }
    executable_records.sort_by(|left, right| left.path.cmp(&right.path));

    let mut components = prepared
        .sources()
        .components
        .values()
        .map(|component| BundleComponent {
            instance: component.instance.clone(),
            dependency_key: component.dependency_key.clone(),
            package_id: component.package_id.clone(),
            package: component.package.clone(),
            source: source_identity(&component.source),
        })
        .collect::<Vec<_>>();
    components.sort_by(|left, right| left.instance.cmp(&right.instance));

    let mut features = options.features.clone();
    features.sort();
    features.dedup();
    let manifest = BundleManifest {
        schema: BUNDLE_SCHEMA.to_owned(),
        robot_id: prepared.document().robot.id.clone(),
        document: prepared.document().clone(),
        root_package: BundlePackage {
            id: prepared.root_package().id.to_string(),
            name: prepared.root_package().name.to_string(),
            source: "local".to_owned(),
        },
        target: options.target.clone().unwrap_or_else(|| "host".to_owned()),
        profile: options.profile.clone().unwrap_or_else(|| "dev".to_owned()),
        features,
        executables: executable_records,
        components,
    };
    let provenance = provenance(prepared)?;
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

fn provenance(prepared: &PreparedProject) -> Result<BundleProvenance, Error> {
    let root = prepared.layout().root();
    let model = prepared
        .document()
        .robot
        .model
        .as_ref()
        .map(|path| -> Result<BundleFile, Error> {
            let relative = safe_input_path(path)?;
            let full = safe_source_file(root, &relative)?;
            let digest = digest_file(&full)?;
            Ok(BundleFile {
                path: relative.to_string_lossy().replace('\\', "/"),
                sha256: digest.sha256,
                bytes: digest.bytes,
            })
        })
        .transpose()?;
    Ok(BundleProvenance {
        schema: BUNDLE_SCHEMA.to_owned(),
        robot_manifest_sha256: digest_file(prepared.layout().robot_manifest())?.sha256,
        cargo_manifest_sha256: digest_file(prepared.layout().cargo_manifest())?.sha256,
        cargo_lock_sha256: optional_digest(&prepared.cargo_lock())?,
        model,
    })
}

fn optional_digest(path: &Path) -> Result<Option<String>, Error> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(digest_file(path)?.sha256)),
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

fn cargo_artifact(stdout: &[u8], target: &SelectedTarget) -> Result<PathBuf, Error> {
    let mut executable = None;
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let message =
            serde_json::from_slice::<Message>(line).map_err(|error| Error::ArtifactCapture {
                package: target.package.clone(),
                target: target.target.clone(),
                message: format!("invalid Cargo JSON message: {error}"),
            })?;
        if let Message::CompilerArtifact(artifact) = message
            && artifact.package_id.to_string() == target.package_id
            && artifact.target.name == target.target
            && artifact.target.is_bin()
        {
            executable = artifact.executable.map(|path| path.into_std_path_buf());
        }
    }
    executable.ok_or_else(|| Error::ArtifactCapture {
        package: target.package.clone(),
        target: target.target.clone(),
        message: "no compiler-artifact executable matched the selected package".to_owned(),
    })
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
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
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

fn source_identity(source: &PackageSource) -> String {
    match source {
        PackageSource::Local { .. } => "local".to_owned(),
        PackageSource::Git { source }
        | PackageSource::Registry { source }
        | PackageSource::Other { source } => source.clone(),
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
            }),
            "local"
        );
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
}
