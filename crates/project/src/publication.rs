//! Isolated Cargo package preparation for registry review.
//!
//! This module owns the local half of the publication workflow. It selects a
//! package from authored Cargo manifests, captures the required source context
//! outside that source tree, invokes Cargo's own packager, and verifies the
//! resulting archive before writing review inventory and checksum records.
//!
//! Remote registry submission is deliberately not implemented here. The dry
//! run result is the exact archive and evidence that a later submission client
//! would upload.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::error::{Error, PublicationError};

/// The publication evidence document generation.
pub const PUBLICATION_SCHEMA: &str = "phoxal/publication/v0";

const GENERATED_LIB: &str = "_cargo/lib.rs";
const INVENTORY_FILE: &str = "review-inventory.json";
const CHECKSUM_FILE: &str = "archive.sha256";
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// A package role accepted by the publication command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicationKind {
    /// A component package, including a passive data-only component.
    Component,
    /// A service implementation or configuration preset package.
    Service,
}

impl PublicationKind {
    /// Returns the command spelling for this package role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Service => "service",
        }
    }
}

impl std::fmt::Display for PublicationKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Arguments controlling one local package publication preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationOptions {
    /// Semantic package role selected by the command.
    pub kind: PublicationKind,
    /// Exact authored Cargo package name.
    pub name: String,
    /// Optional source directory override.
    pub path: Option<PathBuf>,
    /// Keep preparation local and skip remote registry submission.
    pub dry_run: bool,
}

/// One file in the verified Cargo archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationFile {
    /// Package-relative archive path using forward slashes.
    pub path: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Lowercase SHA-256 digest of the file bytes.
    pub sha256: String,
}

/// The local result of a validated publication preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationResult {
    kind: PublicationKind,
    package: String,
    version: String,
    source_root: PathBuf,
    staging_root: PathBuf,
    archive: PathBuf,
    inventory: PathBuf,
    checksum_file: PathBuf,
    checksum: String,
    bytes: u64,
    files: Vec<PublicationFile>,
}

impl PublicationResult {
    /// Returns the selected semantic package role.
    #[must_use]
    pub const fn kind(&self) -> PublicationKind {
        self.kind
    }

    /// Returns the exact Cargo package name.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the authored package version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the selected authored source directory.
    #[must_use]
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    /// Returns the retained isolated staging directory.
    #[must_use]
    pub fn staging_root(&self) -> &Path {
        &self.staging_root
    }

    /// Returns the verified `.crate` archive path.
    #[must_use]
    pub fn archive(&self) -> &Path {
        &self.archive
    }

    /// Returns the JSON review inventory path.
    #[must_use]
    pub fn inventory(&self) -> &Path {
        &self.inventory
    }

    /// Returns the SHA-256 sidecar path.
    #[must_use]
    pub fn checksum_file(&self) -> &Path {
        &self.checksum_file
    }

    /// Returns the verified archive SHA-256 digest.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    /// Returns the archive byte count.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns the sorted verified archive file inventory.
    #[must_use]
    pub fn files(&self) -> &[PublicationFile] {
        &self.files
    }
}

/// Prepares and verifies one local package publication.
///
/// The function currently supports the dry-run half of the workflow. A
/// non-dry-run call fails before creating a staging directory because remote
/// GitHub fork, authorization, and pull-request submission are not yet a
/// supported implementation.
pub fn prepare_publication(options: &PublicationOptions) -> Result<PublicationResult, Error> {
    if !options.dry_run {
        return Err(PublicationError::SubmissionUnavailable.into());
    }
    let selected = select_package(options)?;
    let staging = tempfile::Builder::new()
        .prefix("phoxal-publication-")
        .tempdir()
        .map_err(|source| PublicationError::StagingDirectory { source })?;
    let staging_root = staging.path().to_owned();

    let captured = capture_source(&selected, &staging_root)?;
    let expected_assets = stage_package(&selected, &captured)?;
    let archive = package_with_cargo(&selected.package, &captured)?;
    let archive_bytes = fs::metadata(&archive)
        .map_err(|source| PublicationError::CaptureSource {
            path: archive.clone(),
            source,
        })?
        .len();
    if archive_bytes > MAX_ARCHIVE_BYTES {
        return Err(PublicationError::ArchiveTooLarge {
            path: archive,
            bytes: archive_bytes,
        }
        .into());
    }
    let verified = verify_archive(
        &archive,
        &selected.package,
        &selected.version,
        selected.role,
        &expected_assets,
    )?;

    let inventory_path = staging_root.join(INVENTORY_FILE);
    let inventory = InventoryDocument {
        schema: PUBLICATION_SCHEMA.to_owned(),
        kind: selected.role.publication_kind(),
        package: selected.package.clone(),
        version: selected.version.clone(),
        source_root: "authored".to_owned(),
        archive: archive.display().to_string(),
        checksum: verified.checksum.clone(),
        bytes: verified.bytes,
        files: verified.files.clone(),
    };
    let inventory_json = serde_json::to_vec_pretty(&inventory).map_err(|source| {
        PublicationError::WriteInventory {
            path: inventory_path.clone(),
            source: io::Error::other(source),
        }
    })?;
    fs::write(&inventory_path, inventory_json).map_err(|source| {
        PublicationError::WriteInventory {
            path: inventory_path.clone(),
            source,
        }
    })?;

    let checksum_file = staging_root.join(CHECKSUM_FILE);
    let checksum_text = format!("{}  {}\n", verified.checksum, file_name(&archive)?);
    fs::write(&checksum_file, checksum_text).map_err(|source| PublicationError::WriteChecksum {
        path: checksum_file.clone(),
        source,
    })?;

    let retained_root = staging.keep();
    let archive = relocate_path(&archive, &staging_root, &retained_root);
    let inventory_path = relocate_path(&inventory_path, &staging_root, &retained_root);
    let checksum_file = relocate_path(&checksum_file, &staging_root, &retained_root);
    Ok(PublicationResult {
        kind: selected.role.publication_kind(),
        package: selected.package,
        version: selected.version,
        source_root: selected.source_root,
        staging_root: retained_root,
        archive,
        inventory: inventory_path,
        checksum_file,
        checksum: verified.checksum,
        bytes: verified.bytes,
        files: verified.files,
    })
}

fn relocate_path(path: &Path, old_root: &Path, new_root: &Path) -> PathBuf {
    path.strip_prefix(old_root)
        .map_or_else(|_| path.to_owned(), |relative| new_root.join(relative))
}

#[derive(Debug, Clone)]
struct SelectedPackage {
    package: String,
    version: String,
    source_root: PathBuf,
    manifest: PathBuf,
    workspace: Option<WorkspaceContext>,
    manifest_value: toml::Value,
    role: PackageRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageRole {
    PassiveComponent,
    RustComponent,
    Service,
    Preset,
}

impl PackageRole {
    const fn publication_kind(self) -> PublicationKind {
        match self {
            Self::PassiveComponent | Self::RustComponent => PublicationKind::Component,
            Self::Service | Self::Preset => PublicationKind::Service,
        }
    }
}

impl std::fmt::Display for PackageRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PassiveComponent => "passive component",
            Self::RustComponent => "component",
            Self::Service => "service",
            Self::Preset => "service preset",
        })
    }
}

#[derive(Debug, Clone)]
struct WorkspaceContext {
    root: PathBuf,
    manifest: PathBuf,
    package_relative: PathBuf,
}

fn select_package(options: &PublicationOptions) -> Result<SelectedPackage, Error> {
    if !is_package_name(&options.name) {
        return Err(PublicationError::InvalidName {
            name: options.name.clone(),
        }
        .into());
    }
    let current = env::current_dir().map_err(|source| PublicationError::ResolveSource {
        path: PathBuf::from("."),
        source,
    })?;
    let manifest = if let Some(path) = &options.path {
        let supplied = if path.is_absolute() {
            path.clone()
        } else {
            current.join(path)
        };
        let source_root =
            supplied
                .canonicalize()
                .map_err(|source| PublicationError::ResolveSource {
                    path: supplied.clone(),
                    source,
                })?;
        if !source_root.is_dir() {
            return Err(PublicationError::SourceNotDirectory { path: source_root }.into());
        }
        let manifest = source_root.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(PublicationError::MissingPackageManifest { path: source_root }.into());
        }
        manifest
    } else {
        select_implicit_manifest(&current, &options.name)?
    };
    selected_from_manifest(manifest, options)
}

fn select_implicit_manifest(start: &Path, name: &str) -> Result<PathBuf, Error> {
    let canonical_start =
        start
            .canonicalize()
            .map_err(|source| PublicationError::ResolveSource {
                path: start.to_owned(),
                source,
            })?;
    let mut cursor = if canonical_start.is_file() {
        canonical_start
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| PublicationError::PackageNotFound {
                name: name.to_owned(),
                start: start.to_owned(),
            })?
    } else {
        canonical_start
    };
    let mut workspace_manifest = None;
    loop {
        let manifest = cursor.join("Cargo.toml");
        if manifest.is_file() {
            let value = read_manifest(&manifest)?;
            if value
                .get("package")
                .and_then(toml::Value::as_table)
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                .is_some_and(|package_name| package_name == name)
            {
                return Ok(manifest);
            }
            if value.get("workspace").is_some_and(toml::Value::is_table) {
                workspace_manifest = Some(manifest);
                break;
            }
        }
        if !cursor.pop() {
            break;
        }
    }
    let Some(workspace_manifest) = workspace_manifest else {
        return Err(PublicationError::PackageNotFound {
            name: name.to_owned(),
            start: start.to_owned(),
        }
        .into());
    };
    let workspace_root =
        workspace_manifest
            .parent()
            .ok_or_else(|| PublicationError::PackageNotFound {
                name: name.to_owned(),
                start: start.to_owned(),
            })?;
    let workspace = read_manifest(&workspace_manifest)?;
    let members = workspace_members(workspace_root, &workspace)?;
    let mut matches = members
        .into_iter()
        .filter_map(|member| {
            let manifest = member.join("Cargo.toml");
            let value = read_manifest(&manifest).ok()?;
            let member_name = value
                .get("package")
                .and_then(toml::Value::as_table)
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)?;
            (member_name == name).then_some(manifest)
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    match matches.as_slice() {
        [] => Err(PublicationError::PackageNotFound {
            name: name.to_owned(),
            start: start.to_owned(),
        }
        .into()),
        [manifest] => Ok(manifest.clone()),
        _ => Err(PublicationError::AmbiguousPackage {
            name: name.to_owned(),
            workspace: workspace_root.to_owned(),
            candidates: matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        }
        .into()),
    }
}

fn selected_from_manifest(
    manifest: PathBuf,
    options: &PublicationOptions,
) -> Result<SelectedPackage, Error> {
    let manifest = manifest
        .canonicalize()
        .map_err(|source| PublicationError::ResolveSource {
            path: manifest.clone(),
            source,
        })?;
    let source_root = manifest
        .parent()
        .ok_or_else(|| PublicationError::MissingPackageManifest {
            path: manifest.clone(),
        })?
        .to_owned();
    let manifest_value = read_manifest(&manifest)?;
    let package = manifest_value
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| PublicationError::MissingPackageManifest {
            path: manifest.clone(),
        })?;
    let actual_name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| PublicationError::MissingPackageField {
            path: manifest.clone(),
            field: "name",
        })?;
    if actual_name != options.name {
        return Err(PublicationError::PackageNameMismatch {
            expected: options.name.clone(),
            actual: actual_name.to_owned(),
            path: manifest,
        }
        .into());
    }
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| PublicationError::MissingPackageField {
            path: manifest.clone(),
            field: "version",
        })?
        .to_owned();
    let workspace = find_workspace(&source_root, &manifest)?;
    let role = classify_package(&source_root, &manifest_value, actual_name)?;
    if role.publication_kind() != options.kind {
        return Err(PublicationError::WrongPublicationKind {
            package: actual_name.to_owned(),
            path: source_root,
            actual: role.to_string(),
            requested: options.kind,
        }
        .into());
    }
    Ok(SelectedPackage {
        package: actual_name.to_owned(),
        version,
        source_root,
        manifest,
        workspace,
        manifest_value,
        role,
    })
}

fn read_manifest(path: &Path) -> Result<toml::Value, Error> {
    let text = fs::read_to_string(path).map_err(|source| PublicationError::CaptureSource {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&text)
        .map_err(|source| PublicationError::ParsePublicationManifest {
            path: path.to_owned(),
            source,
        })
        .map_err(Into::into)
}

fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn classify_package(
    source_root: &Path,
    manifest: &toml::Value,
    package: &str,
) -> Result<PackageRole, Error> {
    let package_table = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| PublicationError::MissingPackageManifest {
            path: source_root.join("Cargo.toml"),
        })?;
    let metadata_kind = package_table
        .get("metadata")
        .and_then(toml::Value::as_table)
        .and_then(|metadata| metadata.get("phoxal"))
        .and_then(toml::Value::as_table)
        .and_then(|phoxal| phoxal.get("kind"))
        .and_then(toml::Value::as_str);
    if let Some(kind) = metadata_kind {
        let role = match kind {
            "component" => {
                require_component_definition(source_root, package, manifest)?;
                if has_authored_target(source_root, manifest) {
                    PackageRole::RustComponent
                } else {
                    PackageRole::PassiveComponent
                }
            }
            "service" => {
                if has_authored_target(source_root, manifest) {
                    PackageRole::Service
                } else {
                    return Err(PublicationError::ServiceWithoutTarget {
                        package: package.to_owned(),
                    }
                    .into());
                }
            }
            "preset" => PackageRole::Preset,
            other => {
                return Err(PublicationError::UnsupportedPackageKind {
                    package: package.to_owned(),
                    kind: other.to_owned(),
                }
                .into());
            }
        };
        if role == PackageRole::Preset && has_authored_target(source_root, manifest) {
            return Ok(PackageRole::Service);
        }
        return Ok(role);
    }

    if source_root.join("component.yaml").is_file() {
        require_component_definition(source_root, package, manifest)?;
        return Ok(if has_authored_target(source_root, manifest) {
            PackageRole::RustComponent
        } else {
            PackageRole::PassiveComponent
        });
    }
    if source_root.join("service.yaml").is_file() {
        return if has_authored_target(source_root, manifest) {
            Ok(PackageRole::Service)
        } else {
            Ok(PackageRole::Preset)
        };
    }
    if has_authored_target(source_root, manifest) {
        Ok(PackageRole::Service)
    } else {
        Err(PublicationError::ServiceWithoutTarget {
            package: package.to_owned(),
        }
        .into())
    }
}

fn require_component_definition(
    source_root: &Path,
    package: &str,
    manifest: &toml::Value,
) -> Result<PathBuf, Error> {
    let relative = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("metadata"))
        .and_then(toml::Value::as_table)
        .and_then(|metadata| metadata.get("phoxal"))
        .and_then(toml::Value::as_table)
        .and_then(|phoxal| phoxal.get("definition"))
        .and_then(toml::Value::as_str)
        .unwrap_or("component.yaml");
    let path =
        safe_source_path(source_root, relative).map_err(|_| PublicationError::UnsafeAssetPath {
            reference: relative.to_owned(),
            definition: source_root.join("Cargo.toml"),
            root: source_root.to_owned(),
        })?;
    if !path.is_file() {
        return Err(PublicationError::MissingComponentDefinition {
            package: package.to_owned(),
            path,
        }
        .into());
    }
    Ok(path)
}

fn has_authored_target(source_root: &Path, manifest: &toml::Value) -> bool {
    let Some(package) = manifest.get("package").and_then(toml::Value::as_table) else {
        return false;
    };
    if package
        .get("build")
        .is_some_and(|build| build.as_bool().is_none_or(|value| value))
    {
        return true;
    }
    if ["lib", "bin", "example", "test", "bench"]
        .iter()
        .any(|key| manifest.get(*key).is_some())
    {
        return true;
    }
    let Ok(entries) = walk_files(source_root) else {
        return true;
    };
    entries.iter().any(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
    })
}

fn find_workspace(source_root: &Path, manifest: &Path) -> Result<Option<WorkspaceContext>, Error> {
    let mut cursor = source_root.to_owned();
    loop {
        let candidate = cursor.join("Cargo.toml");
        if candidate == manifest {
            let value = read_manifest(&candidate)?;
            if value.get("workspace").is_some_and(toml::Value::is_table)
                && value.get("package").is_none()
            {
                return Ok(Some(WorkspaceContext {
                    root: cursor,
                    manifest: candidate,
                    package_relative: PathBuf::from("."),
                }));
            }
        } else if candidate.is_file() {
            let value = read_manifest(&candidate)?;
            if value.get("workspace").is_some_and(toml::Value::is_table) {
                let relative = source_root
                    .strip_prefix(&cursor)
                    .map(PathBuf::from)
                    .map_err(|_| PublicationError::CaptureSource {
                        path: source_root.to_owned(),
                        source: io::Error::other("workspace root is not an ancestor"),
                    })?;
                return Ok(Some(WorkspaceContext {
                    root: cursor,
                    manifest: candidate,
                    package_relative: relative,
                }));
            }
        }
        if !cursor.pop() {
            return Ok(None);
        }
    }
}

fn workspace_members(
    workspace_root: &Path,
    workspace: &toml::Value,
) -> Result<Vec<PathBuf>, Error> {
    let table = workspace
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| PublicationError::CaptureSource {
            path: workspace_root.join("Cargo.toml"),
            source: io::Error::other("missing workspace table"),
        })?;
    let patterns = table
        .get("members")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .collect::<Vec<_>>();
    let excluded = table
        .get("exclude")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    let mut members = BTreeSet::new();
    for pattern in patterns {
        for path in expand_member_pattern(workspace_root, pattern)? {
            let relative = path
                .strip_prefix(workspace_root)
                .map(PathBuf::from)
                .unwrap_or_else(|_| path.clone());
            if !excluded.contains(&relative) {
                members.insert(path);
            }
        }
    }
    Ok(members.into_iter().collect())
}

fn expand_member_pattern(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, Error> {
    let pattern_path = Path::new(pattern);
    if pattern_path.is_absolute()
        || pattern_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PublicationError::UnsafeAssetPath {
            reference: pattern.to_owned(),
            definition: root.join("Cargo.toml"),
            root: root.to_owned(),
        }
        .into());
    }
    let components = pattern_path.components().collect::<Vec<_>>();
    let mut paths = vec![root.to_owned()];
    for component in components {
        let Component::Normal(component) = component else {
            continue;
        };
        let component = component.to_string_lossy();
        let wildcard = component.contains('*') || component.contains('?');
        let mut next = Vec::new();
        for path in paths {
            if wildcard {
                let entries =
                    fs::read_dir(&path).map_err(|source| PublicationError::CaptureSource {
                        path: path.clone(),
                        source,
                    })?;
                for entry in entries {
                    let entry = entry.map_err(|source| PublicationError::CaptureSource {
                        path: path.clone(),
                        source,
                    })?;
                    if entry
                        .file_type()
                        .map_err(|source| PublicationError::CaptureSource {
                            path: entry.path(),
                            source,
                        })?
                        .is_dir()
                        && wildcard_match(&component, &entry.file_name().to_string_lossy())
                    {
                        next.push(entry.path());
                    }
                }
            } else {
                next.push(path.join(component.as_ref()));
            }
        }
        paths = next;
    }
    Ok(paths
        .into_iter()
        .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file())
        .collect())
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((b'*', rest)) => {
                inner(rest, value) || (!value.is_empty() && inner(pattern, &value[1..]))
            }
            Some((b'?', rest)) => !value.is_empty() && inner(rest, &value[1..]),
            Some((character, rest)) => {
                value
                    .first()
                    .is_some_and(|value_character| character == value_character)
                    && inner(rest, &value[1..])
            }
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
}

#[derive(Debug, Clone)]
struct CapturedSource {
    manifest: PathBuf,
    workspace_root: PathBuf,
}

fn capture_source(
    selected: &SelectedPackage,
    staging_root: &Path,
) -> Result<CapturedSource, Error> {
    let package_root = if let Some(workspace) = &selected.workspace {
        let root = staging_root.to_owned();
        let workspace_value = read_manifest(&workspace.manifest)?;
        let mut workspace_value = workspace_value;
        let workspace_table = workspace_value
            .get_mut("workspace")
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| PublicationError::CaptureSource {
                path: workspace.manifest.clone(),
                source: io::Error::other("missing workspace table"),
            })?;
        let member = if workspace.package_relative == Path::new(".") {
            ".".to_owned()
        } else {
            workspace.package_relative.display().to_string()
        };
        workspace_table.insert(
            "members".to_owned(),
            toml::Value::Array(vec![toml::Value::String(member)]),
        );
        workspace_table.remove("default-members");
        let workspace_manifest = root.join("Cargo.toml");
        let workspace_text = toml::to_string_pretty(&workspace_value)
            .map_err(|source| PublicationError::SerializeStagedManifest { source })?;
        let root_package = workspace.package_relative == Path::new(".");
        if !root_package {
            write_staged_file(&workspace_manifest, workspace_text.as_bytes())?;
        }
        copy_tree(&workspace.root.join(".cargo"), &root.join(".cargo"), true)?;
        copy_optional_file(&workspace.root.join("Cargo.lock"), &root.join("Cargo.lock"))?;
        let target = root.join(&workspace.package_relative);
        copy_tree(&selected.source_root, &target, false)?;
        if root_package {
            write_staged_file(&workspace_manifest, workspace_text.as_bytes())?;
        }
        capture_path_dependencies(
            &selected.source_root,
            &workspace.root,
            &workspace_value,
            &root,
            &mut BTreeSet::new(),
        )?;
        target
    } else {
        let target = staging_root.to_owned();
        copy_tree(&selected.source_root, &target, false)?;
        capture_path_dependencies(
            &selected.source_root,
            &selected.source_root,
            &toml::Value::Table(toml::map::Map::new()),
            staging_root,
            &mut BTreeSet::new(),
        )?;
        target
    };
    let manifest = package_root.join("Cargo.toml");
    Ok(CapturedSource {
        manifest,
        workspace_root: staging_root.to_owned(),
    })
}

fn copy_optional_file(source: &Path, destination: &Path) -> Result<(), Error> {
    if source.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| PublicationError::CaptureSource {
                path: parent.to_owned(),
                source: error,
            })?;
        }
        fs::copy(source, destination).map_err(|error| PublicationError::CaptureSource {
            path: source.to_owned(),
            source: error,
        })?;
    }
    Ok(())
}

fn write_staged_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PublicationError::WriteStagedFile {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(path, bytes).map_err(|source| PublicationError::WriteStagedFile {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

fn capture_path_dependencies(
    package_root: &Path,
    workspace_root: &Path,
    workspace_manifest: &toml::Value,
    staging_root: &Path,
    captured: &mut BTreeSet<PathBuf>,
) -> Result<(), Error> {
    let manifest_path = package_root.join("Cargo.toml");
    let manifest = read_manifest(&manifest_path)?;
    let mut dependencies = BTreeMap::new();
    collect_dependency_tables(&manifest, &mut dependencies);
    let workspace_dependencies = workspace_manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table);
    for (key, dependency) in dependencies {
        let effective = if dependency.as_table().is_some_and(|table| {
            table.get("workspace").and_then(toml::Value::as_bool) == Some(true)
        }) {
            workspace_dependencies.and_then(|table| table.get(&key))
        } else {
            Some(&dependency)
        };
        let Some(path_value) = effective
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("path"))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        let dependency_base = if effective.is_some_and(|value| std::ptr::eq(value, &dependency)) {
            package_root
        } else {
            workspace_root
        };
        let dependency_root = safe_source_path(dependency_base, path_value).map_err(|_| {
            PublicationError::UnsafeAssetPath {
                reference: path_value.to_owned(),
                definition: manifest_path.clone(),
                root: dependency_base.to_owned(),
            }
        })?;
        if !dependency_root.is_dir() {
            return Err(PublicationError::MissingAsset {
                reference: path_value.to_owned(),
                definition: manifest_path.clone(),
            }
            .into());
        }
        let canonical =
            dependency_root
                .canonicalize()
                .map_err(|source| PublicationError::CaptureSource {
                    path: dependency_root.clone(),
                    source,
                })?;
        if !captured.insert(canonical.clone()) {
            continue;
        }
        let relative = canonical
            .strip_prefix(workspace_root)
            .map(PathBuf::from)
            .map_err(|_| PublicationError::CaptureSource {
                path: canonical.clone(),
                source: io::Error::other("path dependency escapes staging workspace"),
            })?;
        copy_tree(&canonical, &staging_root.join(relative), false)?;
        capture_path_dependencies(
            &canonical,
            workspace_root,
            workspace_manifest,
            staging_root,
            captured,
        )?;
    }
    Ok(())
}

fn collect_dependency_tables(
    manifest: &toml::Value,
    dependencies: &mut BTreeMap<String, toml::Value>,
) {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
            dependencies.extend(
                table
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            collect_dependency_tables(target, dependencies);
        }
    }
}

fn copy_tree(source: &Path, destination: &Path, optional: bool) -> Result<(), Error> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(source_error) if optional && source_error.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(source_error) => {
            return Err(PublicationError::CaptureSource {
                path: source.to_owned(),
                source: source_error,
            }
            .into());
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(PublicationError::SymbolicLink {
            path: source.to_owned(),
        }
        .into());
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| PublicationError::CaptureSource {
                path: parent.to_owned(),
                source: error,
            })?;
        }
        fs::copy(source, destination).map_err(|error| PublicationError::CaptureSource {
            path: source.to_owned(),
            source: error,
        })?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(PublicationError::CaptureSource {
            path: source.to_owned(),
            source: io::Error::other("publication source is not a regular file or directory"),
        }
        .into());
    }
    fs::create_dir_all(destination).map_err(|error| PublicationError::CaptureSource {
        path: destination.to_owned(),
        source: error,
    })?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| PublicationError::CaptureSource {
            path: source.to_owned(),
            source: error,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| PublicationError::CaptureSource {
            path: source.to_owned(),
            source: error,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == ".git" || name == "target" || name == ".codex" {
            continue;
        }
        copy_tree(&entry.path(), &destination.join(name), false)?;
    }
    Ok(())
}

fn stage_package(
    selected: &SelectedPackage,
    captured: &CapturedSource,
) -> Result<BTreeSet<String>, Error> {
    let definition = definition_for_package(selected)?;
    let mut expected = BTreeSet::new();
    if let Some(definition) = definition.as_ref() {
        let relative = definition
            .strip_prefix(&selected.source_root)
            .map_err(|_| PublicationError::UnsafeAssetPath {
                reference: definition.display().to_string(),
                definition: selected.manifest.clone(),
                root: selected.source_root.clone(),
            })?;
        expected.insert(path_string(relative));
        let assets = asset_closure(&selected.source_root, definition)?;
        for asset in assets {
            let relative = asset.strip_prefix(&selected.source_root).map_err(|_| {
                PublicationError::UnsafeAssetPath {
                    reference: asset.display().to_string(),
                    definition: definition.to_owned(),
                    root: selected.source_root.clone(),
                }
            })?;
            expected.insert(path_string(relative));
        }
    }
    if selected.role == PackageRole::PassiveComponent || selected.role == PackageRole::Preset {
        let authored_files = walk_files(&selected.source_root)?;
        if let Some(rust_file) = authored_files.iter().find(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
        }) {
            return Err(PublicationError::TargetlessRustSource {
                package: selected.package.clone(),
                path: rust_file.clone(),
            }
            .into());
        }
        let mut manifest = read_manifest(&captured.manifest)?;
        {
            let package = manifest
                .get_mut("package")
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| PublicationError::MissingPackageManifest {
                    path: selected.manifest.clone(),
                })?;
            package.insert(
                "publish".to_owned(),
                toml::Value::Array(vec![toml::Value::String("phoxal".to_owned())]),
            );
        }
        let staged_manifest = captured.manifest.clone();
        let root =
            staged_manifest
                .parent()
                .ok_or_else(|| PublicationError::MissingPackageManifest {
                    path: staged_manifest.clone(),
                })?;
        let definition = definition.as_ref();
        let mut include = expected
            .iter()
            .cloned()
            .map(toml::Value::String)
            .collect::<Vec<_>>();
        include.push(toml::Value::String(GENERATED_LIB.to_owned()));
        include.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
        include.dedup();
        {
            let manifest_table = manifest.as_table_mut().ok_or_else(|| {
                PublicationError::MissingPackageManifest {
                    path: staged_manifest.clone(),
                }
            })?;
            manifest_table.insert(
                "lib".to_owned(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "path".to_owned(),
                    toml::Value::String(GENERATED_LIB.to_owned()),
                )])),
            );
            let package = manifest_table
                .get_mut("package")
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| PublicationError::MissingPackageManifest {
                    path: staged_manifest.clone(),
                })?;
            package.insert("include".to_owned(), toml::Value::Array(include));
        }
        {
            let package = manifest
                .get_mut("package")
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| PublicationError::MissingPackageManifest {
                    path: selected.manifest.clone(),
                })?;
            let metadata = package
                .entry("metadata".to_owned())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            let phoxal = metadata
                .as_table_mut()
                .ok_or_else(|| PublicationError::MissingPackageManifest {
                    path: staged_manifest.clone(),
                })?
                .entry("phoxal".to_owned())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            let phoxal =
                phoxal
                    .as_table_mut()
                    .ok_or_else(|| PublicationError::MissingPackageManifest {
                        path: staged_manifest.clone(),
                    })?;
            phoxal.insert(
                "kind".to_owned(),
                toml::Value::String(if selected.role == PackageRole::Preset {
                    "preset".to_owned()
                } else {
                    "component".to_owned()
                }),
            );
            if let Some(definition) = definition {
                phoxal.insert(
                    "definition".to_owned(),
                    toml::Value::String(path_string(
                        definition
                            .strip_prefix(&selected.source_root)
                            .map_err(|_| PublicationError::UnsafeAssetPath {
                                reference: definition.display().to_string(),
                                definition: selected.manifest.clone(),
                                root: selected.source_root.clone(),
                            })?,
                    )),
                );
            }
        }
        let text = toml::to_string_pretty(&manifest)
            .map_err(|source| PublicationError::SerializeStagedManifest { source })?;
        fs::write(&staged_manifest, text).map_err(|source| PublicationError::WriteStagedFile {
            path: staged_manifest.clone(),
            source,
        })?;
        let generated = root.join(GENERATED_LIB);
        if let Some(parent) = generated.parent() {
            fs::create_dir_all(parent).map_err(|source| PublicationError::WriteStagedFile {
                path: parent.to_owned(),
                source,
            })?;
        }
        fs::write(
            &generated,
            "//! Tool-generated inert carrier for a passive Phoxal package.\n#![no_std]\n\n",
        )
        .map_err(|source| PublicationError::WriteStagedFile {
            path: generated,
            source,
        })?;
        expected.insert(GENERATED_LIB.to_owned());
    }
    Ok(expected)
}

fn definition_for_package(selected: &SelectedPackage) -> Result<Option<PathBuf>, Error> {
    match selected.role {
        PackageRole::PassiveComponent | PackageRole::RustComponent => {
            Ok(Some(require_component_definition(
                &selected.source_root,
                &selected.package,
                &selected.manifest_value,
            )?))
        }
        PackageRole::Service => Ok(None),
        PackageRole::Preset => {
            let path = selected.source_root.join("service.yaml");
            Ok(path.is_file().then_some(path))
        }
    }
}

fn asset_closure(root: &Path, definition: &Path) -> Result<BTreeSet<PathBuf>, Error> {
    let text =
        fs::read_to_string(definition).map_err(|source| PublicationError::CaptureSource {
            path: definition.to_owned(),
            source,
        })?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|source| PublicationError::MissingAsset {
            reference: source.to_string(),
            definition: definition.to_owned(),
        })?;
    let mut assets = BTreeSet::new();
    collect_yaml_assets(root, definition, &value, false, &mut assets)?;
    let conventional_model = root.join("model.xml");
    if conventional_model.is_file() {
        assets.insert(conventional_model);
    }
    Ok(assets)
}

fn collect_yaml_assets(
    root: &Path,
    definition: &Path,
    value: &serde_yaml::Value,
    path_hint: bool,
    assets: &mut BTreeSet<PathBuf>,
) -> Result<(), Error> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            for (key, value) in mapping {
                let hint =
                    key.as_str().is_some_and(is_asset_key) || (path_hint && value.is_sequence());
                collect_yaml_assets(root, definition, value, hint, assets)?;
            }
        }
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                collect_yaml_assets(root, definition, value, path_hint, assets)?;
            }
        }
        serde_yaml::Value::String(reference) if path_hint || looks_like_asset(reference) => {
            if reference.trim().is_empty() {
                return Ok(());
            }
            let path = safe_source_path(root, reference).map_err(|_| {
                PublicationError::UnsafeAssetPath {
                    reference: reference.clone(),
                    definition: definition.to_owned(),
                    root: root.to_owned(),
                }
            })?;
            if !path.exists() {
                return Err(PublicationError::MissingAsset {
                    reference: reference.clone(),
                    definition: definition.to_owned(),
                }
                .into());
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| PublicationError::CaptureSource {
                    path: path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(PublicationError::SymbolicLink { path }.into());
            }
            if metadata.is_dir() {
                for child in walk_files(&path)? {
                    assets.insert(child);
                }
            } else if metadata.is_file() {
                assets.insert(path);
            } else {
                return Err(PublicationError::MissingAsset {
                    reference: reference.clone(),
                    definition: definition.to_owned(),
                }
                .into());
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_asset_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "asset",
        "assets",
        "file",
        "files",
        "model",
        "mesh",
        "meshes",
        "texture",
        "textures",
        "scene",
        "path",
        "paths",
        "uri",
        "uris",
        "readme",
        "license-file",
    ]
    .iter()
    .any(|candidate| key == *candidate || key.ends_with(&format!("_{candidate}")))
}

fn looks_like_asset(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        ".xml", ".urdf", ".mjcf", ".mjz", ".obj", ".mtl", ".glb", ".gltf", ".stl", ".png", ".jpg",
        ".jpeg", ".json", ".yaml", ".yml", ".bin",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

fn safe_source_path(root: &Path, reference: &str) -> Result<PathBuf, ()> {
    let relative = Path::new(reference);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(());
    }
    let path = root.join(relative);
    let canonical_root = root.canonicalize().map_err(|_| ())?;
    if path.exists() {
        let canonical_path = path.canonicalize().map_err(|_| ())?;
        canonical_path
            .starts_with(canonical_root)
            .then_some(canonical_path)
            .ok_or(())
    } else {
        Ok(path)
    }
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let metadata =
        fs::symlink_metadata(root).map_err(|source| PublicationError::CaptureSource {
            path: root.to_owned(),
            source,
        })?;
    if metadata.file_type().is_symlink() {
        return Err(PublicationError::SymbolicLink {
            path: root.to_owned(),
        }
        .into());
    }
    if metadata.is_file() {
        return Ok(vec![root.to_owned()]);
    }
    let mut entries = fs::read_dir(root)
        .map_err(|source| PublicationError::CaptureSource {
            path: root.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| PublicationError::CaptureSource {
            path: root.to_owned(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.path();
        if entry.file_name() == ".git"
            || entry.file_name() == "target"
            || entry.file_name() == ".codex"
        {
            continue;
        }
        files.extend(walk_files(&path)?);
    }
    Ok(files)
}

fn package_with_cargo(package: &str, captured: &CapturedSource) -> Result<PathBuf, Error> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let target = captured.workspace_root.join("target");
    let mut command = Command::new(cargo);
    command
        .current_dir(&captured.workspace_root)
        .args([
            "package",
            "--manifest-path",
            &captured.manifest.display().to_string(),
            "--allow-dirty",
            "--target-dir",
            &target.display().to_string(),
        ])
        .env_remove("CARGO_REGISTRIES_PHOXAL_TOKEN")
        .env_remove("CARGO_REGISTRY_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("PHOXAL_GITHUB_TOKEN");
    let output = command
        .output()
        .map_err(|source| PublicationError::CargoPackage {
            package: package.to_owned(),
            message: source.to_string(),
        })?;
    if !output.status.success() {
        let mut message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if message.is_empty() {
            message = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        }
        return Err(PublicationError::CargoPackage {
            package: package.to_owned(),
            message,
        }
        .into());
    }
    let archive = target.join("package").join(format!(
        "{package}-{}.crate",
        package_version(&captured.manifest)?
    ));
    if !archive.is_file() {
        return Err(PublicationError::MissingArchive {
            package: package.to_owned(),
            path: archive,
        }
        .into());
    }
    Ok(archive)
}

fn package_version(manifest: &Path) -> Result<String, Error> {
    read_manifest(manifest)?
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            PublicationError::MissingPackageField {
                path: manifest.to_owned(),
                field: "version",
            }
            .into()
        })
}

#[derive(Debug, Clone)]
struct VerifiedArchive {
    checksum: String,
    bytes: u64,
    files: Vec<PublicationFile>,
}

#[derive(Debug, Serialize)]
struct InventoryDocument {
    schema: String,
    kind: PublicationKind,
    package: String,
    version: String,
    source_root: String,
    archive: String,
    checksum: String,
    bytes: u64,
    files: Vec<PublicationFile>,
}

fn verify_archive(
    archive_path: &Path,
    package: &str,
    version: &str,
    role: PackageRole,
    expected_assets: &BTreeSet<String>,
) -> Result<VerifiedArchive, Error> {
    let archive_bytes = fs::metadata(archive_path)
        .map_err(|source| PublicationError::CaptureSource {
            path: archive_path.to_owned(),
            source,
        })?
        .len();
    if archive_bytes > MAX_ARCHIVE_BYTES {
        return Err(PublicationError::ArchiveTooLarge {
            path: archive_path.to_owned(),
            bytes: archive_bytes,
        }
        .into());
    }
    let checksum = archive_checksum(archive_path)?;
    let root_name = format!("{package}-{version}");
    let file = File::open(archive_path).map_err(|source| PublicationError::CaptureSource {
        path: archive_path.to_owned(),
        source,
    })?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let mut paths = BTreeSet::new();
    let mut files = Vec::new();
    let mut cargo_manifest = None;
    let mut total = 0_u64;
    let entries = archive
        .entries()
        .map_err(|source| invalid_archive(archive_path, source))?;
    for entry in entries {
        let mut entry = entry.map_err(|source| invalid_archive(archive_path, source))?;
        let path = entry
            .path()
            .map_err(|source| invalid_archive(archive_path, source))?
            .into_owned();
        let relative = validate_archive_path(&path, &root_name).map_err(|message| {
            PublicationError::InvalidArchive {
                path: archive_path.to_owned(),
                message,
            }
        })?;
        let relative_string = path_string(&relative);
        if !paths.insert(path.clone()) {
            return Err(PublicationError::InvalidArchive {
                path: archive_path.to_owned(),
                message: format!("duplicate archive path {relative_string}"),
            }
            .into());
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(PublicationError::InvalidArchive {
                path: archive_path.to_owned(),
                message: format!("archive path {relative_string} is a link"),
            }
            .into());
        }
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            return Err(PublicationError::InvalidArchive {
                path: archive_path.to_owned(),
                message: format!("archive path {relative_string} is not a regular file"),
            }
            .into());
        }
        let entry_bytes = entry.size();
        if entry_bytes > MAX_ENTRY_BYTES || total.saturating_add(entry_bytes) > MAX_ARCHIVE_BYTES {
            return Err(PublicationError::ArchiveTooLarge {
                path: archive_path.to_owned(),
                bytes: total.saturating_add(entry_bytes),
            }
            .into());
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut read_bytes = 0_u64;
        let mut manifest_bytes = Vec::new();
        loop {
            let count = entry
                .read(&mut buffer)
                .map_err(|source| invalid_archive(archive_path, source))?;
            if count == 0 {
                break;
            }
            read_bytes += count as u64;
            if read_bytes > entry_bytes || read_bytes > MAX_ENTRY_BYTES {
                return Err(PublicationError::InvalidArchive {
                    path: archive_path.to_owned(),
                    message: format!("archive entry {relative_string} exceeds its declared size"),
                }
                .into());
            }
            hasher.update(&buffer[..count]);
            if relative_string == "Cargo.toml" {
                if manifest_bytes.len() + count > MAX_MANIFEST_BYTES as usize {
                    return Err(PublicationError::InvalidArchive {
                        path: archive_path.to_owned(),
                        message: "normalized Cargo.toml is too large".to_owned(),
                    }
                    .into());
                }
                manifest_bytes.extend_from_slice(&buffer[..count]);
            }
        }
        if read_bytes != entry_bytes {
            return Err(PublicationError::InvalidArchive {
                path: archive_path.to_owned(),
                message: format!("archive entry {relative_string} was truncated"),
            }
            .into());
        }
        total += read_bytes;
        if relative_string == "Cargo.toml" {
            cargo_manifest = Some(manifest_bytes);
        }
        files.push(PublicationFile {
            path: relative_string,
            bytes: read_bytes,
            sha256: format!("{:x}", hasher.finalize()),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let Some(cargo_manifest) = cargo_manifest else {
        return Err(PublicationError::InvalidArchive {
            path: archive_path.to_owned(),
            message: "archive has no normalized Cargo.toml".to_owned(),
        }
        .into());
    };
    let manifest = toml::from_slice::<toml::Value>(&cargo_manifest).map_err(|source| {
        PublicationError::InvalidArchive {
            path: archive_path.to_owned(),
            message: format!("normalized Cargo.toml is invalid: {source}"),
        }
    })?;
    let archived_package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str);
    let archived_version = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str);
    if archived_package != Some(package) || archived_version != Some(version) {
        return Err(PublicationError::InvalidArchive {
            path: archive_path.to_owned(),
            message: format!(
                "archive identity is {:?} {:?}, expected {package} {version}",
                archived_package, archived_version
            ),
        }
        .into());
    }
    let archived_paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    for expected in expected_assets {
        if !archived_paths.contains(expected.as_str()) {
            return Err(PublicationError::MissingArchivedAsset {
                package: package.to_owned(),
                asset: expected.clone(),
            }
            .into());
        }
    }
    if matches!(role, PackageRole::PassiveComponent | PackageRole::Preset)
        && !archived_paths.contains(GENERATED_LIB)
    {
        return Err(PublicationError::MissingArchivedAsset {
            package: package.to_owned(),
            asset: GENERATED_LIB.to_owned(),
        }
        .into());
    }
    Ok(VerifiedArchive {
        checksum,
        bytes: archive_bytes,
        files,
    })
}

fn archive_checksum(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path).map_err(|source| PublicationError::CaptureSource {
        path: path.to_owned(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| PublicationError::CaptureSource {
                path: path.to_owned(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn invalid_archive(path: &Path, source: impl std::fmt::Display) -> PublicationError {
    PublicationError::InvalidArchive {
        path: path.to_owned(),
        message: source.to_string(),
    }
}

fn validate_archive_path(path: &Path, root_name: &str) -> Result<PathBuf, String> {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(root)) if root == root_name => {}
        Some(Component::Normal(root)) => {
            return Err(format!("archive root {root:?} does not match {root_name}"));
        }
        _ => return Err("archive path is absolute or has no package root".to_owned()),
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(component) => relative.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("unsafe archive path {path:?}"));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err("archive contains an empty package path".to_owned());
    }
    Ok(relative)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn file_name(path: &Path) -> Result<String, Error> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            PublicationError::InvalidArchive {
                path: path.to_owned(),
                message: "archive path has no file name".to_owned(),
            }
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)
    }

    #[test]
    fn exact_package_selection_does_not_use_fuzzy_names() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        write(
            &directory.path().join("Cargo.toml"),
            "[package]\nname = \"exact-package\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        let options = PublicationOptions {
            kind: PublicationKind::Service,
            name: "exact-packag".to_owned(),
            path: Some(directory.path().to_owned()),
            dry_run: true,
        };
        let error = select_package(&options).expect_err("fuzzy names must fail");
        assert!(matches!(
            error,
            Error::Publication(PublicationError::PackageNameMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn asset_paths_reject_parent_escape_and_missing_files() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let definition = directory.path().join("component.yaml");
        write(
            &definition,
            "schema: phoxal/component/v0\nassets: [../outside.obj]\n",
        )?;
        let error =
            asset_closure(directory.path(), &definition).expect_err("parent escape must fail");
        assert!(matches!(
            error,
            Error::Publication(PublicationError::UnsafeAssetPath { .. })
        ));

        write(
            &definition,
            "schema: phoxal/component/v0\nassets: [missing.obj]\n",
        )?;
        let error =
            asset_closure(directory.path(), &definition).expect_err("missing asset must fail");
        assert!(matches!(
            error,
            Error::Publication(PublicationError::MissingAsset { .. })
        ));
        Ok(())
    }

    #[test]
    fn wildcard_workspace_member_selection_is_deterministic()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        write(
            &directory.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"services/*\"]\n",
        )?;
        for (member, name) in [("one", "one-service"), ("two", "two-service")] {
            write(
                &directory
                    .path()
                    .join(format!("services/{member}/Cargo.toml")),
                &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
            )?;
        }
        let members = workspace_members(
            directory.path(),
            &read_manifest(&directory.path().join("Cargo.toml"))?,
        )?;
        assert_eq!(members.len(), 2);
        assert!(members[0].ends_with("services/one"));
        Ok(())
    }

    #[test]
    fn passive_dry_run_packages_only_declared_content_and_keeps_authored_tree_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        write(
            &directory.path().join("Cargo.toml"),
            "[package]\nname = \"passive-caster\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"Passive caster\"\nlicense = \"MIT\"\n",
        )?;
        write(
            &directory.path().join("component.yaml"),
            "schema: phoxal/component/v0\nassets: [assets/model.txt]\n",
        )?;
        write(&directory.path().join("assets/model.txt"), "model\n")?;
        write(&directory.path().join("unrelated.txt"), "do not package\n")?;
        let before = snapshot_tree(directory.path())?;
        let result = prepare_publication(&PublicationOptions {
            kind: PublicationKind::Component,
            name: "passive-caster".to_owned(),
            path: Some(directory.path().to_owned()),
            dry_run: true,
        })?;
        let after = snapshot_tree(directory.path())?;
        assert_eq!(before, after);
        assert!(result.archive().is_file());
        assert!(result.inventory().is_file());
        assert!(result.checksum_file().is_file());
        assert!(result.files().iter().any(|file| file.path == GENERATED_LIB));
        assert!(
            result
                .files()
                .iter()
                .any(|file| file.path == "component.yaml")
        );
        assert!(
            result
                .files()
                .iter()
                .any(|file| file.path == "assets/model.txt")
        );
        assert!(
            !result
                .files()
                .iter()
                .any(|file| file.path == "unrelated.txt")
        );
        let inventory: serde_json::Value = serde_json::from_slice(&fs::read(result.inventory())?)?;
        assert_eq!(inventory["checksum"].as_str(), Some(result.checksum()));
        Ok(())
    }

    #[test]
    fn real_service_dry_run_preserves_library_and_binary_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        write(
            &directory.path().join("Cargo.toml"),
            "[package]\nname = \"example-service\"\nversion = \"0.2.0\"\nedition = \"2024\"\ndescription = \"Example service\"\nlicense = \"MIT\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"example-service\"\npath = \"src/main.rs\"\n",
        )?;
        write(
            &directory.path().join("src/lib.rs"),
            "pub struct Service;\n",
        )?;
        write(&directory.path().join("src/main.rs"), "fn main() {}\n")?;
        let result = prepare_publication(&PublicationOptions {
            kind: PublicationKind::Service,
            name: "example-service".to_owned(),
            path: Some(directory.path().to_owned()),
            dry_run: true,
        })?;
        assert_eq!(result.kind(), PublicationKind::Service);
        assert!(result.files().iter().any(|file| file.path == "src/lib.rs"));
        assert!(result.files().iter().any(|file| file.path == "src/main.rs"));
        assert!(!result.files().iter().any(|file| file.path == GENERATED_LIB));
        Ok(())
    }

    #[test]
    fn workspace_member_publication_captures_inherited_manifest_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        write(
            &directory.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"services/example\"]\n[workspace.package]\nedition = \"2024\"\nlicense = \"MIT\"\n",
        )?;
        write(
            &directory.path().join("services/example/Cargo.toml"),
            "[package]\nname = \"workspace-service\"\nversion = \"0.3.0\"\nedition.workspace = true\nlicense.workspace = true\n\n[[bin]]\nname = \"workspace-service\"\npath = \"src/main.rs\"\n",
        )?;
        write(
            &directory.path().join("services/example/src/main.rs"),
            "fn main() {}\n",
        )?;
        let result = prepare_publication(&PublicationOptions {
            kind: PublicationKind::Service,
            name: "workspace-service".to_owned(),
            path: Some(directory.path().join("services/example")),
            dry_run: true,
        })?;
        assert!(result.files().iter().any(|file| file.path == "src/main.rs"));
        Ok(())
    }

    fn snapshot_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, io::Error> {
        let mut snapshot = BTreeMap::new();
        for path in walk_files(root).map_err(|error| io::Error::other(error.to_string()))? {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| io::Error::other(error.to_string()))?;
            snapshot.insert(path_string(relative), fs::read(path)?);
        }
        Ok(snapshot)
    }
}
