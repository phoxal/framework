//! Reading the supervisor's sole persisted input, and locating the run
//! directory that owns it.
//!
//! Opening a source bundle admits `manifest.json`, validates its exact
//! executable records, and stops before launching anything. The supervisor
//! later launches only that admitted graph. The legacy observer bundle remains
//! available for existing observer fixtures while the typed runtime transport
//! is completed.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::bundle::RuntimeBundle;
use crate::model::manifest::ManifestDocument;
use crate::participant::metadata::ParticipantKind;

/// The bundle directory's name inside a deployment release. The supervisor is
/// handed a bundle root and knows nothing about releases, but it does have to
/// find the run directory that owns the execution, and a release's bundle
/// always sits one level inside the release.
const RELEASE_BUNDLE_DIR: &str = "bundle";

/// Where a source project keeps its own release, relative to the project root.
const PROJECT_RELEASE_SUFFIX: [&str; 2] = [".phoxal", "release"];

/// Read the bundle at `root`.
#[derive(Debug)]
pub(crate) enum Bundle {
    /// The pre-runtime bundle retained for existing observer fixtures.
    Legacy(RuntimeBundle),
    /// The source compiler's executable bundle, which this supervisor owns.
    Source(SourceBundle),
}

impl Bundle {
    pub(crate) fn root(&self) -> &Path {
        match self {
            Self::Legacy(bundle) => bundle.root(),
            Self::Source(bundle) => bundle.root(),
        }
    }

    pub(crate) fn legacy_manifest(&self) -> Option<ManifestDocument> {
        match self {
            Self::Legacy(bundle) => Some(bundle.manifest().clone()),
            Self::Source(_) => None,
        }
    }

    pub(crate) fn robot_id(&self) -> &str {
        match self {
            Self::Legacy(bundle) => bundle.robot_id().as_str(),
            Self::Source(bundle) => &bundle.manifest.robot_id,
        }
    }

    pub(crate) fn services(&self) -> Vec<String> {
        match self {
            Self::Legacy(bundle) => bundle
                .robot()
                .services()
                .map(|(id, _)| id.as_str().to_owned())
                .collect(),
            Self::Source(bundle) => bundle
                .manifest
                .executables
                .iter()
                .filter(|entry| entry.role == "service")
                .map(|entry| entry.instance.clone())
                .collect(),
        }
    }

    pub(crate) fn expected_processes(&self) -> Vec<(String, ParticipantKind)> {
        match self {
            Self::Legacy(bundle) => {
                let mut processes = vec![("brain".to_owned(), ParticipantKind::Brain)];
                processes.extend(
                    bundle
                        .robot()
                        .services()
                        .map(|(id, _)| (id.as_str().to_owned(), ParticipantKind::Service)),
                );
                processes.extend(bundle.robot().components().filter_map(|component| {
                    component
                        .instance()
                        .driver()
                        .map(|_| (component.id().as_str().to_owned(), ParticipantKind::Driver))
                }));
                processes
            }
            Self::Source(bundle) => bundle
                .manifest
                .executables
                .iter()
                .map(|entry| {
                    (
                        entry.instance.clone(),
                        match entry.role.as_str() {
                            "brain" => ParticipantKind::Brain,
                            "service" => ParticipantKind::Service,
                            "driver" => ParticipantKind::Driver,
                            _ => ParticipantKind::Service,
                        },
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn source(&self) -> Option<&SourceBundle> {
        match self {
            Self::Source(bundle) => Some(bundle),
            Self::Legacy(_) => None,
        }
    }
}

/// A source compiler bundle admitted for exact process launch.
#[derive(Debug, Clone)]
pub(crate) struct SourceBundle {
    root: PathBuf,
    manifest: SourceManifest,
}

impl SourceBundle {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn executables(&self) -> impl Iterator<Item = &SourceExecutable> {
        self.manifest.executables.iter()
    }

}

/// Open either the legacy observer fixture or a source-side `bundle/v0`.
pub(crate) fn open(root: &Path) -> Result<Bundle> {
    let root = root.canonicalize().with_context(|| {
        format!(
            "phoxal-supervisor takes a compiled bundle directory; {} is not one",
            root.display()
        )
    })?;
    if !root.is_dir() {
        bail!("compiled bundle root is not a directory: {}", root.display());
    }
    let manifest_path = root.join(crate::bundle::MANIFEST_FILE);
    let bytes = bounded_file(&manifest_path, MAX_MANIFEST_BYTES)?;
    if let Ok(manifest) = serde_json::from_slice::<SourceManifest>(&bytes)
        && manifest.schema == SOURCE_SCHEMA
    {
        return Ok(Bundle::Source(admit_source(root, manifest)?));
    }
    RuntimeBundle::open(&root)
        .map(Bundle::Legacy)
        .with_context(|| format!("{} is not a supported compiled bundle", root.display()))
}

const SOURCE_SCHEMA: &str = "phoxal/bundle/v0";
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceManifest {
    pub(crate) schema: String,
    pub(crate) robot_id: String,
    pub(crate) document: SourceDocument,
    root_package: SourcePackage,
    target: String,
    profile: String,
    features: Vec<String>,
    pub(crate) executables: Vec<SourceExecutable>,
    components: Vec<SourceComponentRecord>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceDocument {
    #[serde(default)]
    schema: Option<String>,
    robot: SourceRobot,
    #[serde(default)]
    brain: Option<serde_json::Value>,
    #[serde(default)]
    services: BTreeMap<String, SourceService>,
    #[serde(default)]
    connections: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SourceService {
    #[serde(default)]
    implementation: Option<String>,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRobot {
    id: String,
    #[serde(default)]
    model: Option<std::path::PathBuf>,
    #[serde(default)]
    kinematic: Option<serde_json::Value>,
    #[serde(default)]
    motion_limits: Option<serde_json::Value>,
    #[serde(default)]
    components: BTreeMap<String, SourceComponent>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceComponent {
    component: String,
    mount_link: String,
    #[serde(default)]
    driver: Option<serde_json::Value>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePackage {
    id: String,
    name: String,
    source: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceComponentRecord {
    instance: String,
    dependency_key: String,
    package_id: String,
    package: String,
    source: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceExecutable {
    pub(crate) role: String,
    pub(crate) instance: String,
    package_id: String,
    package: String,
    target: String,
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
    artifact: Option<serde_json::Value>,
}

impl SourceExecutable {
    pub(crate) fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        instance: impl Into<String>,
        path: impl Into<String>,
        bytes: u64,
        sha256: impl Into<String>,
    ) -> Self {
        let instance = instance.into();
        Self {
            role: if instance == "brain" {
                "brain".to_owned()
            } else {
                "service".to_owned()
            },
            instance,
            package_id: "fixture".to_owned(),
            package: "fixture".to_owned(),
            target: "fixture".to_owned(),
            path: path.into(),
            bytes,
            sha256: sha256.into(),
            artifact: None,
        }
    }
}

impl SourceBundle {
    #[cfg(test)]
    pub(crate) fn for_test(root: &Path, manifest: SourceManifest) -> Self {
        Self {
            root: root.to_owned(),
            manifest,
        }
    }
}

#[cfg(test)]
impl SourceManifest {
    pub(crate) fn for_test(robot_id: impl Into<String>, executables: Vec<SourceExecutable>) -> Self {
        Self {
            schema: SOURCE_SCHEMA.to_owned(),
            robot_id: robot_id.into(),
            document: SourceDocument {
                schema: Some("phoxal/robot/v0".to_owned()),
                robot: SourceRobot {
                    id: "fixture".to_owned(),
                    model: None,
                    kinematic: None,
                    motion_limits: None,
                    components: BTreeMap::new(),
                },
                brain: None,
                services: BTreeMap::new(),
                connections: BTreeMap::new(),
            },
            root_package: SourcePackage {
                id: "fixture".to_owned(),
                name: "fixture".to_owned(),
                source: "local".to_owned(),
            },
            target: "host".to_owned(),
            profile: "dev".to_owned(),
            features: Vec::new(),
            executables,
            components: Vec::new(),
        }
    }
}

fn admit_source(root: PathBuf, manifest: SourceManifest) -> Result<SourceBundle> {
    if manifest.schema != SOURCE_SCHEMA {
        bail!("unsupported bundle schema `{}`", manifest.schema);
    }
    validate_segment(&manifest.robot_id, "robot_id")?;
    validate_source_document(&manifest)?;
    validate_source_metadata(&manifest)?;
    if manifest.executables.is_empty() {
        bail!("source bundle contains no executable records");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut has_brain = false;
    for executable in &manifest.executables {
        if !matches!(executable.role.as_str(), "brain" | "service" | "driver") {
            bail!("unsupported executable role `{}`", executable.role);
        }
        validate_segment(&executable.instance, "executable instance")?;
        if !seen.insert(executable.instance.as_str()) {
            bail!("source bundle contains duplicate executable instance `{}`", executable.instance);
        }
        if executable.role == "brain" {
            if executable.instance != "brain" || has_brain {
                bail!("source bundle must contain exactly one executable brain");
            }
            has_brain = true;
        } else if executable.instance == "brain" {
            bail!("only the brain role may use executable instance `brain`");
        }
        if executable.package_id.is_empty() {
            bail!(
                "executable `{}` has an empty Cargo package identity",
                executable.instance
            );
        }
        if executable.package.is_empty() {
            bail!(
                "executable `{}` has an empty Cargo package name",
                executable.instance
            );
        }
        if executable.target.is_empty() {
            bail!(
                "executable `{}` has an empty Cargo target name",
                executable.instance
            );
        }
        if executable.sha256.len() != 64
            || !executable
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            bail!(
                "executable `{}` has an invalid lowercase SHA-256 digest",
                executable.instance
            );
        }
        if executable
            .artifact
            .as_ref()
            .is_some_and(|artifact| !artifact.is_object())
        {
            bail!(
                "executable `{}` has a non-object artifact contract",
                executable.instance
            );
        }
        let relative = safe_relative_path(&executable.path)?;
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).with_context(|| {
            format!("source bundle executable is missing: {}", path.display())
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("source bundle executable is not a regular file: {}", path.display());
        }
        let canonical = path.canonicalize().with_context(|| {
            format!("cannot resolve source bundle executable: {}", path.display())
        })?;
        if !canonical.starts_with(&root) {
            bail!("source bundle executable escapes its root: {}", path.display());
        }
        verify_digest(&canonical, executable)?;
    }
    if !has_brain {
        bail!("source bundle is missing executable instance `brain`");
    }
    for (service, definition) in &manifest.document.services {
        validate_segment(service, "service instance")?;
        if service == "brain" {
            bail!("source bundle services cannot contain `brain`");
        }
        if definition.config.as_ref().is_some_and(serde_json::Value::is_null) {
            bail!("service `{service}` has an explicit null configuration");
        }
        if !seen.contains(service.as_str()) {
            bail!("service `{service}` has no executable record");
        }
    }
    Ok(SourceBundle { root, manifest })
}

fn validate_source_document(manifest: &SourceManifest) -> Result<()> {
    if let Some(schema) = &manifest.document.schema
        && schema != "phoxal/robot/v0"
    {
        bail!("unsupported authored document schema `{schema}`");
    }
    if manifest.document.robot.id != manifest.robot_id {
        bail!(
            "bundle robot_id `{}` does not match document robot.id `{}`",
            manifest.robot_id,
            manifest.document.robot.id
        );
    }
    validate_segment(&manifest.document.robot.id, "document robot.id")?;
    // These values are intentionally opaque to the supervisor, but reading
    // them here makes the complete source document part of admission rather
    // than an accidentally ignored subset of the serialized contract.
    let _ = (
        &manifest.document.robot.model,
        &manifest.document.robot.kinematic,
        &manifest.document.robot.motion_limits,
        &manifest.document.brain,
        &manifest.document.connections,
    );
    for (instance, component) in &manifest.document.robot.components {
        validate_segment(instance, "component instance")?;
        if component.component.is_empty() {
            bail!("component `{instance}` has an empty dependency key");
        }
        if component.mount_link.is_empty() {
            bail!("component `{instance}` has an empty mount link");
        }
        let _ = (&component.driver, &component.config);
    }
    for (service, definition) in &manifest.document.services {
        validate_segment(service, "service instance")?;
        if definition
            .implementation
            .as_deref()
            .is_some_and(str::is_empty)
        {
            bail!("service `{service}` has an empty implementation key");
        }
        if definition
            .binary
            .as_deref()
            .is_some_and(str::is_empty)
        {
            bail!("service `{service}` has an empty binary target");
        }
    }
    Ok(())
}

fn validate_source_metadata(manifest: &SourceManifest) -> Result<()> {
    if manifest.root_package.id.is_empty()
        || manifest.root_package.name.is_empty()
        || manifest.root_package.source.is_empty()
    {
        bail!("source bundle root package metadata is incomplete");
    }
    if manifest.target.is_empty() || manifest.profile.is_empty() {
        bail!("source bundle target and profile metadata must not be empty");
    }
    if manifest.features.iter().any(String::is_empty) {
        bail!("source bundle features must not contain empty names");
    }
    let mut seen = std::collections::BTreeSet::new();
    for component in &manifest.components {
        validate_segment(&component.instance, "component instance")?;
        if !seen.insert(component.instance.as_str()) {
            bail!(
                "source bundle contains duplicate component instance `{}`",
                component.instance
            );
        }
        if component.dependency_key.is_empty()
            || component.package_id.is_empty()
            || component.package.is_empty()
            || component.source.is_empty()
        {
            bail!(
                "source bundle component `{}` metadata is incomplete",
                component.instance
            );
        }
    }
    Ok(())
}

fn bounded_file(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let link_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect bundle manifest {}", path.display()))?;
    if link_metadata.file_type().is_symlink() {
        bail!("bundle manifest must not be a symbolic link: {}", path.display());
    }
    let file = fs::File::open(path)
        .with_context(|| format!("cannot read bundle manifest {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("bundle manifest is not a regular file: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        bail!("bundle manifest exceeds {maximum} bytes");
    }
    Ok(bytes)
}

fn validate_segment(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("{label} must be 1-64 lowercase ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
        })
    {
        bail!("executable path `{value}` is not bundle-relative");
    }
    Ok(path.to_owned())
}

fn verify_digest(path: &Path, expected: &SourceExecutable) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| anyhow::anyhow!("executable size overflows u64"))?;
        hasher.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if bytes != expected.bytes || digest != expected.sha256 {
        bail!(
            "executable `{}` does not match its recorded size or SHA-256",
            expected.instance
        );
    }
    Ok(())
}

/// The root whose volatile run directory owns this bundle.
///
/// A bundle inside a deployment release is owned by whatever owns the release:
/// a project keeps its release at `<project>/.phoxal/release`, and every other
/// release - an installed one under `/var/phoxal`, or an extracted archive - is
/// its own root. A bare bundle root, run outside any release, owns itself.
pub(crate) fn owning_root(bundle_root: &Path) -> PathBuf {
    let Some(release_root) = strip_tail(bundle_root, &[RELEASE_BUNDLE_DIR]) else {
        return bundle_root.to_path_buf();
    };
    strip_tail(&release_root, &PROJECT_RELEASE_SUFFIX).unwrap_or(release_root)
}

/// `path` without `tail`, or `None` when it does not end with it.
fn strip_tail(path: &Path, tail: &[&str]) -> Option<PathBuf> {
    let mut components = path.components().rev();
    let found: Vec<_> = components
        .by_ref()
        .take(tail.len())
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<_> = tail.iter().rev().map(ToString::to_string).collect();
    (found == expected).then(|| components.rev().collect::<PathBuf>())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use sha2::Digest;

    use super::*;

    #[test]
    fn a_directory_without_a_manifest_is_not_a_bundle() {
        let dir = tempfile::tempdir().expect("temporary directory");
        std::fs::write(dir.path().join("robot.yaml"), "schema: phoxal/robot/v0\n")
            .expect("source fixture");
        let error = open(dir.path()).expect_err("authored YAML is not a compiled bundle");
        assert!(format!("{error:#}").contains("manifest.json"), "{error:#}");
    }

    /// The run directory a bundle's execution belongs to, for each shape a
    /// bundle root arrives in. The installed cases matter most: the unit starts
    /// `/var/phoxal/phoxal-supervisor /var/phoxal/bundle`, and the execution's
    /// socket and locks belong to the release, never to a directory inside the
    /// immutable release itself.
    #[test]
    fn a_bundle_is_owned_by_whatever_owns_the_release_it_sits_in() {
        assert_eq!(
            owning_root(Path::new("/work/rover/.phoxal/release/bundle")),
            Path::new("/work/rover")
        );
        assert_eq!(
            owning_root(Path::new("/var/phoxal/bundle")),
            Path::new("/var/phoxal")
        );
        assert_eq!(
            owning_root(Path::new("/var/lib/phoxal/releases/current/bundle")),
            Path::new("/var/lib/phoxal/releases/current")
        );
        // A bundle root that is not inside a release owns itself.
        assert_eq!(
            owning_root(Path::new("/var/lib/phoxal/releases/current")),
            Path::new("/var/lib/phoxal/releases/current")
        );
    }

    #[test]
    fn a_source_bundle_admits_the_exact_manifest_and_executable_digest() {
        let directory = tempfile::tempdir().expect("temporary source bundle");
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).expect("bundle bin directory");
        let executable = bin.join("brain");
        let executable_bytes = b"#!/bin/sh\nexit 0\n";
        fs::write(&executable, executable_bytes).expect("bundle executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make bundle executable runnable");
        let digest = format!("{:x}", Sha256::digest(executable_bytes));
        let manifest = format!(
            r#"{{
                "schema": "phoxal/bundle/v0",
                "robot_id": "fixture",
                "document": {{
                    "schema": "phoxal/robot/v0",
                    "robot": {{
                        "id": "fixture",
                        "model": null,
                        "kinematic": null,
                        "motion_limits": null,
                        "components": {{}}
                    }},
                    "brain": null,
                    "services": {{}},
                    "connections": {{}}
                }},
                "root_package": {{
                    "id": "path+file:///fixture#fixture@0.1.0",
                    "name": "fixture",
                    "source": "local"
                }},
                "target": "host",
                "profile": "dev",
                "features": [],
                "executables": [{{
                    "role": "brain",
                    "instance": "brain",
                    "package_id": "path+file:///fixture#fixture@0.1.0",
                    "package": "fixture",
                    "target": "fixture",
                    "path": "bin/brain",
                    "bytes": {},
                    "sha256": "{}",
                    "artifact": null
                }}],
                "components": []
            }}"#,
            executable_bytes.len(),
            digest
        );
        fs::write(directory.path().join("manifest.json"), manifest)
            .expect("source bundle manifest");

        let bundle = open(directory.path()).expect("source bundle admission");
        let Bundle::Source(bundle) = bundle else {
            panic!("the phoxal/bundle/v0 manifest must select source admission");
        };
        assert_eq!(bundle.manifest.robot_id, "fixture");
        assert_eq!(
            bundle
                .executables()
                .next()
                .expect("brain executable")
                .path(),
            Path::new("bin/brain")
        );
    }
}
