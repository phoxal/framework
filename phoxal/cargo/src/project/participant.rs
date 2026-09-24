//! Exact participant installation and prepared Protobuf source publication.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

mod update_index;
use update_index::newest_eligible;

use fs4::TryLockError;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::cargo::CargoOptions;
use super::document::{RobotDocument, ServiceSource};
use super::file_lock::ExclusiveFileLock;
use super::selection::PackageSource;
use super::{Error, ProjectLayout};

const PHOXAL_INDEX: &str = "sparse+https://phoxal.github.io/registry/";

#[derive(Debug, Clone)]
pub(crate) struct InstalledSelection {
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) binary: String,
    pub(crate) executable: PathBuf,
    pub(crate) source_root: PathBuf,
    pub(crate) source: PackageSource,
}

pub(crate) fn selected_installations(
    layout: &ProjectLayout,
    document: &RobotDocument,
    options: &CargoOptions,
) -> Result<BTreeMap<String, InstalledSelection>, Error> {
    let home = phoxal_home()?;
    let target = options.target.clone().map_or_else(host_target, Ok)?;
    let RobotDocument::V0 {
        robot, services, ..
    } = document;
    let mut selected = BTreeMap::new();
    for (instance, selection) in services {
        selected.insert(
            instance.clone(),
            selected_installation(
                layout,
                &home,
                &target,
                &selection.package,
                &selection.version,
                selection.binary.as_deref(),
                selection.source.as_ref(),
            )?,
        );
    }
    for (instance, component) in &robot.components {
        if component.driver.is_some() {
            selected.insert(
                instance.clone(),
                selected_installation(
                    layout,
                    &home,
                    &target,
                    &component.package,
                    &component.version,
                    component.binary.as_deref(),
                    component.source.as_ref(),
                )?,
            );
        }
    }
    Ok(selected)
}

fn selected_installation(
    layout: &ProjectLayout,
    home: &Path,
    target: &str,
    package: &str,
    version: &str,
    binary: Option<&str>,
    source: Option<&ServiceSource>,
) -> Result<InstalledSelection, Error> {
    let binary = binary.unwrap_or(package);
    let (store, source) = match source {
        None => (
            home.join("packages/registry/phoxal")
                .join(package)
                .join(version)
                .join(target),
            PackageSource::Registry {
                source: PHOXAL_INDEX.to_owned(),
            },
        ),
        Some(ServiceSource::Registry(registry)) => (
            home.join("packages/registry")
                .join(&registry.registry)
                .join(package)
                .join(version)
                .join(target),
            PackageSource::Registry {
                source: registry.registry.clone(),
            },
        ),
        Some(ServiceSource::Git(git)) => (
            home.join("packages/git")
                .join(package)
                .join(&git.rev)
                .join(version)
                .join(target),
            PackageSource::Git {
                source: format!("git+{}#{}", git.git, git.rev),
            },
        ),
        Some(ServiceSource::Path(path)) => {
            let authored = layout
                .root()
                .join(&path.path)
                .canonicalize()
                .map_err(|source| Error::ArtifactFile {
                    path: layout.root().join(&path.path),
                    source,
                })?;
            let digest = Sha256::digest(authored.to_string_lossy().as_bytes());
            let identity = digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            (
                home.join("packages/local")
                    .join(package)
                    .join(identity)
                    .join(version)
                    .join(target),
                PackageSource::Local {
                    manifest_path: authored.join("Cargo.toml"),
                },
            )
        }
    };
    let executable = store.join("bin").join(binary);
    let source_root = store.join("source");
    if !executable.is_file() || !source_root.join("Cargo.toml").is_file() {
        return Err(invalid(
            layout.robot_manifest(),
            format!("{package} {version} is not prepared; run `cargo phoxal prepare`"),
        ));
    }
    Ok(InstalledSelection {
        package: package.to_owned(),
        version: version.to_owned(),
        binary: binary.to_owned(),
        executable,
        source_root,
        source,
    })
}

#[derive(Deserialize)]
struct Robot {
    #[serde(default)]
    services: BTreeMap<String, Selection>,
    robot: RobotSection,
}

#[derive(Deserialize)]
struct RobotSection {
    #[serde(default)]
    components: BTreeMap<String, Component>,
}

#[derive(Deserialize)]
struct Component {
    #[serde(flatten)]
    selection: Selection,
    #[serde(default)]
    driver: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct Selection {
    package: String,
    version: String,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Source {
    Path(PathSource),
    Git(GitSource),
    Registry(RegistrySource),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathSource {
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitSource {
    git: String,
    rev: String,
    path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrySource {
    registry: String,
}

/// Prepares the exact executable and source closure declared by `robot.yaml`.
pub(crate) fn prepare(
    layout: &ProjectLayout,
    options: &CargoOptions,
) -> Result<Vec<String>, Error> {
    options.validate()?;
    let source = fs::read(layout.robot_manifest()).map_err(|source| Error::ReadRobot {
        path: layout.robot_manifest().to_owned(),
        source,
    })?;
    let robot: Robot = serde_yaml::from_slice(&source).map_err(|source| Error::ParseRobot {
        path: layout.robot_manifest().to_owned(),
        source,
    })?;
    prepare_robot(layout, options, robot)
}

fn prepare_robot(
    layout: &ProjectLayout,
    options: &CargoOptions,
    robot: Robot,
) -> Result<Vec<String>, Error> {
    let _lock = preparation_lock(layout)?;
    let home = phoxal_home()?;
    let _installation_lock = installation_lock(&home)?;
    let target = match &options.target {
        Some(target) => target.clone(),
        None => host_target()?,
    };
    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    for (instance, selection) in robot.services {
        if !seen.insert(format!("{selection:?}")) {
            continue;
        }
        if let Some(change) =
            prepare_selection(layout, options, &home, &target, &instance, &selection)?
        {
            changes.push(change);
        }
    }
    for (instance, component) in robot.robot.components {
        if component.driver.is_some()
            && seen.insert(format!("{:?}", component.selection))
            && let Some(change) = prepare_selection(
                layout,
                options,
                &home,
                &target,
                &instance,
                &component.selection,
            )?
        {
            changes.push(change);
        }
    }
    Ok(changes)
}

/// Proposes or applies newer exact registry selections already in `robot.yaml`.
pub(crate) fn update(
    layout: &ProjectLayout,
    options: &CargoOptions,
    dry_run: bool,
    role: Option<&str>,
    instance: Option<&str>,
) -> Result<Vec<String>, Error> {
    options.validate()?;
    if options.selection != super::cargo::CargoSelection::default()
        || options.profile.is_some()
        || !options.features.is_empty()
        || options.all_features
        || options.no_default_features
        || options.release
        || options.message_format.is_some()
        || !options.cargo_args.is_empty()
        || !options.test_args.is_empty()
    {
        return Err(Error::InvalidOptions {
            message: "update accepts participant selection, --dry-run, Cargo path, target, and offline or lock mode only".to_owned(),
        });
    }
    let original = fs::read(layout.robot_manifest()).map_err(|source| Error::ReadRobot {
        path: layout.robot_manifest().to_owned(),
        source,
    })?;
    let original_text = std::str::from_utf8(&original)
        .map_err(|error| invalid(layout.robot_manifest(), error.to_string()))?;
    super::document::parse_and_validate(original_text, layout.robot_manifest())?;
    let robot: Robot = serde_yaml::from_slice(&original).map_err(|source| Error::ParseRobot {
        path: layout.robot_manifest().to_owned(),
        source,
    })?;
    if role.is_some() != instance.is_some()
        || role.is_some_and(|role| role != "service" && role != "component")
    {
        return Err(invalid(
            layout.robot_manifest(),
            "select `service <instance>` or `component <instance>`",
        ));
    }
    let mut proposed: serde_yaml::Value =
        serde_yaml::from_slice(&original).map_err(|source| Error::ParseRobot {
            path: layout.robot_manifest().to_owned(),
            source,
        })?;
    let mut changes = Vec::new();
    let mut found = false;
    for (name, selection) in &robot.services {
        if role.is_some() && (role != Some("service") || instance != Some(name)) {
            continue;
        }
        found = true;
        match newest_eligible(layout, options, selection)? {
            Some(version) if version != selection.version => {
                proposed["services"][name]["version"] = serde_yaml::Value::String(version.clone());
                changes.push(format!(
                    "service {name}: {} {} -> {version}",
                    selection.package, selection.version
                ));
            }
            _ => changes.push(format!(
                "service {name}: {} {} {}",
                selection.package,
                selection.version,
                if matches!(
                    &selection.source,
                    Some(Source::Git(_)) | Some(Source::Path(_))
                ) {
                    "manually selected"
                } else {
                    "unchanged"
                }
            )),
        }
    }
    for (name, component) in &robot.robot.components {
        if role.is_some() && (role != Some("component") || instance != Some(name)) {
            continue;
        }
        found = true;
        match newest_eligible(layout, options, &component.selection)? {
            Some(version) if version != component.selection.version => {
                proposed["robot"]["components"][name]["version"] =
                    serde_yaml::Value::String(version.clone());
                changes.push(format!(
                    "component {name}: {} {} -> {version}",
                    component.selection.package, component.selection.version
                ));
            }
            _ => changes.push(format!(
                "component {name}: {} {} {}",
                component.selection.package,
                component.selection.version,
                if matches!(
                    &component.selection.source,
                    Some(Source::Git(_)) | Some(Source::Path(_))
                ) {
                    "manually selected"
                } else {
                    "unchanged"
                }
            )),
        }
    }
    if !found && role.is_some() {
        return Err(invalid(
            layout.robot_manifest(),
            format!("selected {role:?} {instance:?} is not declared"),
        ));
    }
    if dry_run || !changes.iter().any(|change| change.contains(" -> ")) {
        return Ok(changes);
    }
    let encoded = serde_yaml::to_string(&proposed)
        .map_err(|error| invalid(layout.robot_manifest(), error.to_string()))?;
    super::document::parse_and_validate(&encoded, layout.robot_manifest())?;
    let staged: Robot = serde_yaml::from_str(&encoded)
        .map_err(|error| invalid(layout.robot_manifest(), error.to_string()))?;
    prepare_robot(layout, options, staged)?;
    let validation = tempfile::tempdir().map_err(|source| Error::ArtifactFile {
        path: std::env::temp_dir(),
        source,
    })?;
    phoxal_build::validate_project_api(layout.root(), encoded.as_bytes(), validation.path())
        .map_err(|error| {
            invalid(
                layout.robot_manifest(),
                format!("updated API composition is invalid: {error}"),
            )
        })?;
    if fs::read(layout.robot_manifest()).map_err(|source| Error::ReadRobot {
        path: layout.robot_manifest().to_owned(),
        source,
    })? != original
    {
        return Err(invalid(
            layout.robot_manifest(),
            "robot.yaml changed during update; retry",
        ));
    }
    super::preparation::atomic_write(layout.robot_manifest(), encoded.as_bytes()).map_err(
        |source| Error::ArtifactFile {
            path: layout.robot_manifest().to_owned(),
            source,
        },
    )?;
    Ok(changes)
}

fn prepare_selection(
    layout: &ProjectLayout,
    options: &CargoOptions,
    home: &Path,
    target: &str,
    instance: &str,
    selection: &Selection,
) -> Result<Option<String>, Error> {
    let manifest = layout.robot_manifest();
    if !identifier(instance) || !identifier(&selection.package) {
        return Err(invalid(
            manifest,
            format!(
                "invalid participant `{instance}` or package `{}`",
                selection.package
            ),
        ));
    }
    let version = semver::Version::parse(&selection.version).map_err(|error| {
        invalid(
            manifest,
            format!("{instance} needs an exact semantic version: {error}"),
        )
    })?;
    if version.to_string() != selection.version {
        return Err(invalid(
            manifest,
            format!("{instance} version must be canonical and exact"),
        ));
    }
    let binary = selection.binary.as_deref().unwrap_or(&selection.package);
    if !identifier(binary) {
        return Err(invalid(
            manifest,
            format!("{instance} has an invalid binary target"),
        ));
    }
    let (store, prepared) = match &selection.source {
        None => (
            home.join("packages/registry/phoxal")
                .join(&selection.package)
                .join(&selection.version)
                .join(target),
            Some(
                layout
                    .root()
                    .join(".phoxal/registry/phoxal")
                    .join(&selection.package)
                    .join(&selection.version)
                    .join("api"),
            ),
        ),
        Some(Source::Registry(source)) => {
            if !identifier(&source.registry) {
                return Err(invalid(
                    manifest,
                    format!("{instance} has an invalid registry"),
                ));
            }
            (
                home.join("packages/registry")
                    .join(&source.registry)
                    .join(&selection.package)
                    .join(&selection.version)
                    .join(target),
                Some(
                    layout
                        .root()
                        .join(".phoxal/registry")
                        .join(&source.registry)
                        .join(&selection.package)
                        .join(&selection.version)
                        .join("api"),
                ),
            )
        }
        Some(Source::Git(source)) => {
            if source.git.trim().is_empty()
                || source.rev.len() != 40
                || !source.rev.bytes().all(|byte| byte.is_ascii_hexdigit())
                || source
                    .path
                    .as_ref()
                    .is_some_and(|path| !safe_relative_path(path))
            {
                return Err(invalid(
                    manifest,
                    format!("{instance} needs a Git URL and complete commit"),
                ));
            }
            (
                home.join("packages/git")
                    .join(&selection.package)
                    .join(&source.rev)
                    .join(&selection.version)
                    .join(target),
                Some(
                    layout
                        .root()
                        .join(".phoxal/git")
                        .join(&selection.package)
                        .join(&source.rev)
                        .join(&selection.version)
                        .join("api"),
                ),
            )
        }
        Some(Source::Path(source)) => {
            if source.path.as_os_str().is_empty() || !source.path.is_relative() {
                return Err(invalid(
                    manifest,
                    format!("{instance} local source must be a nonempty relative path"),
                ));
            }
            let absolute = layout
                .root()
                .join(&source.path)
                .canonicalize()
                .map_err(|error| Error::ArtifactFile {
                    path: layout.root().join(&source.path),
                    source: error,
                })?;
            let digest = Sha256::digest(absolute.to_string_lossy().as_bytes());
            let identity = digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            (
                home.join("packages/local")
                    .join(&selection.package)
                    .join(identity)
                    .join(&selection.version)
                    .join(target),
                None,
            )
        }
    };
    let installed = store.join("bin").join(binary);
    let complete_install = installed.is_file()
        && store.join(".crates.toml").is_file()
        && store.join("source/Cargo.toml").is_file()
        && (matches!(&selection.source, Some(Source::Path(_)))
            || store.join("source/Cargo.lock").is_file());
    if complete_install && prepared.as_ref().is_some_and(|path| path.is_dir()) {
        return Ok(None);
    }
    // Local package inputs and their local Cargo dependency closure are mutable.
    let local_snapshot = match &selection.source {
        Some(Source::Path(source)) => Some(source_tree(&layout.root().join(&source.path))?),
        _ => None,
    };
    let local_digest = match (&selection.source, &local_snapshot) {
        (Some(Source::Path(source)), Some(snapshot)) => Some(local_install_digest(
            &layout.root().join(&source.path),
            snapshot,
            options,
        )?),
        _ => None,
    };
    if complete_install
        && prepared.is_none()
        && let Some(digest) = &local_digest
        && fs::read(store.join("source-digest")).ok().as_deref() == Some(digest.as_bytes())
    {
        return Ok(None);
    }

    let staging = home.join("packages/.staging");
    fs::create_dir_all(&staging).map_err(|source| Error::ArtifactFile {
        path: staging.clone(),
        source,
    })?;
    let install = tempfile::tempdir_in(&staging).map_err(|source| Error::ArtifactFile {
        path: staging.clone(),
        source,
    })?;
    let mut command = Command::new(options.cargo_program());
    command.args(["install", "--locked", "--message-format", "json", "--root"]);
    command.arg(install.path());
    if let Some(config) = super::cargo::registry_config(layout.root()) {
        command.arg("--config");
        command.arg(config);
    }
    command.args(["--bin", binary]);
    if let Some(target) = &options.target {
        command.args(["--target", target]);
    }
    if options.offline || matches!(options.lock, super::cargo::LockMode::Frozen) {
        command.arg("--offline");
    }
    match &selection.source {
        None => {
            command.args(["--registry", "phoxal"]);
            command.args([
                "--version",
                &format!("={}", selection.version),
                &selection.package,
            ]);
        }
        Some(Source::Registry(source)) => {
            command.args(["--registry", &source.registry]);
            command.args([
                "--version",
                &format!("={}", selection.version),
                &selection.package,
            ]);
        }
        Some(Source::Git(source)) => {
            command.args([
                "--git",
                &source.git,
                "--rev",
                &source.rev,
                &selection.package,
            ]);
        }
        Some(Source::Path(source)) => {
            command.arg("--path");
            command.arg(layout.root().join(&source.path));
        }
    }
    let output = command.output().map_err(|source| Error::CargoSpawn {
        operation: format!("install {} {}", selection.package, selection.version),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::CargoCommand {
            operation: format!("install {} {}", selection.package, selection.version),
            status: output
                .status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let source_root = captured_source(
        &output.stdout,
        binary,
        &selection.package,
        &selection.version,
    )?;
    if let Some(before) = &local_snapshot
        && &source_tree(&source_root)? != before
    {
        return Err(invalid(
            &source_root,
            format!("{instance} local source changed during installation; retry preparation"),
        ));
    }
    if let (Some(before), Some(snapshot)) = (&local_digest, &local_snapshot)
        && local_install_digest(&source_root, snapshot, options)? != *before
    {
        return Err(invalid(
            &source_root,
            format!("{instance} local dependency changed during installation; retry preparation"),
        ));
    }
    if let Some(Source::Git(GitSource {
        path: Some(path), ..
    })) = &selection.source
        && !source_root.ends_with(path)
    {
        return Err(invalid(
            &source_root,
            format!(
                "Git package {} is not at selected path {}",
                selection.package,
                path.display()
            ),
        ));
    }
    let api_source = source_root.join("api");
    if !api_source.is_dir() {
        return Err(invalid(
            &api_source,
            format!("{} has no packaged api/ directory", selection.package),
        ));
    }
    let validation = tempfile::tempdir_in(&staging).map_err(|source| Error::ArtifactFile {
        path: staging.clone(),
        source,
    })?;
    phoxal_build::validate_participant_api(&api_source, validation.path())
        .map_err(|error| invalid(&api_source, format!("invalid participant API: {error}")))?;
    retain_package_files(&source_root, install.path())?;
    if let Some(digest) = &local_digest {
        fs::write(install.path().join("source-digest"), digest).map_err(|source| {
            Error::ArtifactFile {
                path: install.path().join("source-digest"),
                source,
            }
        })?;
    }
    if !install.path().join("bin").join(binary).is_file() {
        return Err(invalid(
            install.path(),
            format!("Cargo did not install binary `{binary}`"),
        ));
    }
    if let Some(destination) = &prepared {
        publish_api(&api_source, destination)?;
    }
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::ArtifactFile {
            path: parent.to_owned(),
            source,
        })?;
        if store.exists() {
            let retired = tempfile::tempdir_in(parent).map_err(|source| Error::ArtifactFile {
                path: parent.to_owned(),
                source,
            })?;
            let previous = retired.path().join("previous");
            fs::rename(&store, &previous).map_err(|source| Error::ArtifactFile {
                path: store.clone(),
                source,
            })?;
            if let Err(source) = fs::rename(install.path(), &store) {
                fs::rename(&previous, &store).map_err(|restore| {
                    invalid(
                        &store,
                        format!(
                            "cannot publish installation: {source}; cannot restore previous installation: {restore}"
                        ),
                    )
                })?;
                return Err(Error::ArtifactFile {
                    path: store,
                    source,
                });
            }
            return Ok(Some(format!(
                "{instance} {} {}",
                selection.package, selection.version
            )));
        }
    }
    fs::rename(install.path(), &store).map_err(|source| Error::ArtifactFile {
        path: store.clone(),
        source,
    })?;
    Ok(Some(format!(
        "{instance} {} {}",
        selection.package, selection.version
    )))
}

fn source_tree_digest(files: &BTreeMap<PathBuf, [u8; 32]>) -> String {
    let mut digest = Sha256::new();
    for (path, hash) in files {
        digest.update(path.to_string_lossy().as_bytes());
        digest.update(hash);
    }
    format!("{:x}", digest.finalize())
}

fn local_install_digest(
    source: &Path,
    files: &BTreeMap<PathBuf, [u8; 32]>,
    options: &CargoOptions,
) -> Result<String, Error> {
    let mut metadata_options = CargoOptions {
        cargo_path: options.cargo_path.clone(),
        lock: options.lock,
        offline: options.offline,
        target: options.target.clone(),
        ..CargoOptions::default()
    };
    // Cargo installs participant defaults independently of root feature flags.
    metadata_options.features.clear();
    let manifest = source.join("Cargo.toml");
    let metadata = super::cargo::load_metadata_at(&manifest, source, None, &metadata_options)?;
    let root = metadata
        .root_package()
        .ok_or_else(|| invalid(&manifest, "local participant is not a Cargo package"))?;
    let resolved = metadata
        .resolve
        .as_ref()
        .ok_or_else(|| invalid(&manifest, "Cargo returned no dependency resolution"))?;
    let mut stack = vec![root.id.clone()];
    let mut seen = BTreeSet::new();
    let mut local = BTreeMap::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(node) = resolved.nodes.iter().find(|node| node.id == id) {
            stack.extend(node.deps.iter().map(|dependency| dependency.pkg.clone()));
        }
        let Some(package) = metadata.packages.iter().find(|package| package.id == id) else {
            continue;
        };
        if package.id == root.id || package.source.is_some() {
            continue;
        }
        let dependency_root = package
            .manifest_path
            .as_std_path()
            .parent()
            .ok_or_else(|| invalid(&manifest, "local dependency has no source root"))?;
        local.insert(
            package.id.to_string(),
            source_tree_digest(&source_tree(dependency_root)?),
        );
    }
    let mut digest = Sha256::new();
    digest.update(source_tree_digest(files));
    for (package, tree) in local {
        digest.update(package);
        digest.update(tree);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn captured_source(
    output: &[u8],
    binary: &str,
    package: &str,
    version: &str,
) -> Result<PathBuf, Error> {
    for line in output.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact" || value["target"]["name"] != binary {
            continue;
        }
        if !value["target"]["kind"]
            .as_array()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        {
            continue;
        }
        let Some(source) = value["target"]["src_path"].as_str() else {
            continue;
        };
        for directory in Path::new(source).ancestors().skip(1) {
            let manifest = directory.join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let text = fs::read_to_string(&manifest).map_err(|source| Error::ReadManifest {
                path: manifest.clone(),
                source,
            })?;
            let document =
                toml::from_str::<toml::Value>(&text).map_err(|source| Error::ParseManifest {
                    path: manifest.clone(),
                    source,
                })?;
            if document["package"]["name"].as_str() == Some(package)
                && document["package"]["version"].as_str() == Some(version)
            {
                return Ok(directory.to_owned());
            }
        }
    }
    Err(invalid(
        Path::new("Cargo.toml"),
        format!(
            "Cargo did not report a binary artifact for exact package {package} {version} ({binary})"
        ),
    ))
}

fn publish_api(source: &Path, destination: &Path) -> Result<(), Error> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::ArtifactFile {
            path: parent.to_owned(),
            source,
        })?;
        let staging = tempfile::tempdir_in(parent).map_err(|source| Error::ArtifactFile {
            path: parent.to_owned(),
            source,
        })?;
        let candidate = staging.path().join("api");
        copy_protos(source, &candidate)?;
        if destination.exists() {
            if same_tree(&candidate, destination)? {
                return Ok(());
            }
            return Err(invalid(
                destination,
                "prepared source differs for the same exact package; remove the corrupted directory before retrying",
            ));
        }
        fs::rename(&candidate, destination).map_err(|source| Error::ArtifactFile {
            path: destination.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn copy_protos(source: &Path, destination: &Path) -> Result<(), Error> {
    fs::create_dir_all(destination).map_err(|error| Error::ArtifactFile {
        path: destination.to_owned(),
        source: error,
    })?;
    for entry in fs::read_dir(source).map_err(|error| Error::ArtifactFile {
        path: source.to_owned(),
        source: error,
    })? {
        let entry = entry.map_err(|error| Error::ArtifactFile {
            path: source.to_owned(),
            source: error,
        })?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&path).map_err(|error| Error::ArtifactFile {
            path: path.clone(),
            source: error,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(invalid(&path, "api/ must not contain symlinks"));
        }
        if metadata.is_dir() {
            copy_protos(&path, &target)?;
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "proto")
        {
            fs::copy(&path, &target).map_err(|error| Error::ArtifactFile {
                path: target.clone(),
                source: error,
            })?;
        } else {
            return Err(invalid(
                &path,
                "api/ may contain only .proto files and directories",
            ));
        }
    }
    Ok(())
}

fn same_tree(left: &Path, right: &Path) -> Result<bool, Error> {
    let mut left_files = BTreeMap::new();
    let mut right_files = BTreeMap::new();
    collect_files(left, left, &mut left_files)?;
    collect_files(right, right, &mut right_files)?;
    Ok(left_files == right_files)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), Error> {
    for entry in fs::read_dir(directory).map_err(|source| Error::ArtifactFile {
        path: directory.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::ArtifactFile {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| Error::ArtifactFile {
            path: path.clone(),
            source,
        })?;
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| invalid(&path, error.to_string()))?;
            files.insert(
                relative.to_owned(),
                fs::read(&path).map_err(|source| Error::ArtifactFile {
                    path: path.clone(),
                    source,
                })?,
            );
        } else {
            return Err(invalid(&path, "prepared api/ has a non-file entry"));
        }
    }
    Ok(())
}

fn source_tree(root: &Path) -> Result<BTreeMap<PathBuf, [u8; 32]>, Error> {
    let mut files = BTreeMap::new();
    collect_source_files(root, root, &mut files)?;
    if let Some(lock) = root
        .ancestors()
        .map(|directory| directory.join("Cargo.lock"))
        .find(|path| path.is_file())
    {
        files.insert(PathBuf::from("Cargo.lock"), digest_file(&lock)?);
    }
    Ok(files)
}

fn collect_source_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, [u8; 32]>,
) -> Result<(), Error> {
    for entry in fs::read_dir(directory).map_err(|source| Error::ArtifactFile {
        path: directory.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::ArtifactFile {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| Error::ArtifactFile {
            path: path.clone(),
            source,
        })?;
        if metadata.is_dir() {
            if ["target", ".git", ".codex", ".phoxal"]
                .iter()
                .any(|skip| entry.file_name() == *skip)
                || (path != root && path.join("Cargo.toml").is_file())
            {
                continue;
            }
            collect_source_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| invalid(&path, error.to_string()))?;
            files.insert(relative.to_owned(), digest_file(&path)?);
        } else {
            return Err(invalid(
                &path,
                "local package source contains a symlink or special file",
            ));
        }
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<[u8; 32], Error> {
    let mut file = File::open(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| Error::ArtifactFile {
                path: path.to_owned(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().into())
}

fn retain_package_files(source: &Path, installed: &Path) -> Result<(), Error> {
    let retained = installed.join("source");
    copy_package_source(source, &retained)
}

fn copy_package_source(source: &Path, retained: &Path) -> Result<(), Error> {
    fs::create_dir_all(retained).map_err(|error| Error::ArtifactFile {
        path: retained.to_owned(),
        source: error,
    })?;
    for entry in fs::read_dir(source).map_err(|error| Error::ArtifactFile {
        path: source.to_owned(),
        source: error,
    })? {
        let entry = entry.map_err(|error| Error::ArtifactFile {
            path: source.to_owned(),
            source: error,
        })?;
        let name = entry.file_name();
        if name == ".cargo-ok" || name == ".cargo-checksum.json" {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| Error::ArtifactFile {
            path: path.clone(),
            source: error,
        })?;
        if metadata.is_dir() {
            if ["target", ".git", ".codex", ".phoxal"]
                .iter()
                .any(|skip| name == *skip)
            {
                continue;
            }
            copy_package_source(&path, &retained.join(&name))?;
        } else if metadata.is_file() {
            fs::copy(&path, retained.join(&name)).map_err(|error| Error::ArtifactFile {
                path,
                source: error,
            })?;
        } else {
            return Err(invalid(
                &path,
                "package source contains a symlink or special file",
            ));
        }
    }
    Ok(())
}

fn preparation_lock(layout: &ProjectLayout) -> Result<ExclusiveFileLock, Error> {
    let directory = layout.root().join("target/phoxal");
    let path = directory.join("preparation.lock");
    fs::create_dir_all(&directory).map_err(|source| Error::ArtifactFile {
        path: directory.clone(),
        source,
    })?;
    let file: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| Error::ArtifactFile {
            path: path.clone(),
            source,
        })?;
    match ExclusiveFileLock::try_acquire(file) {
        Ok(lock) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(invalid(
            &path,
            "another cargo phoxal command is preparing this project",
        )),
        Err(TryLockError::Error(error)) => Err(invalid(&path, error.to_string())),
    }
}

fn installation_lock(home: &Path) -> Result<ExclusiveFileLock, Error> {
    let directory = home.join("packages");
    fs::create_dir_all(&directory).map_err(|source| Error::ArtifactFile {
        path: directory.clone(),
        source,
    })?;
    let path = directory.join(".installation.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| Error::ArtifactFile {
            path: path.clone(),
            source,
        })?;
    ExclusiveFileLock::acquire(file).map_err(|source| Error::ArtifactFile { path, source })
}

fn phoxal_home() -> Result<PathBuf, Error> {
    if let Some(path) = std::env::var_os("PHOXAL_HOME") {
        if path.is_empty() {
            return Err(invalid(
                Path::new("PHOXAL_HOME"),
                "PHOXAL_HOME cannot be empty",
            ));
        }
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| invalid(Path::new("HOME"), "HOME is unavailable; set PHOXAL_HOME"))?;
    #[cfg(target_os = "macos")]
    {
        Ok(PathBuf::from(home).join("Library/Application Support/Phoxal"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".local/share"))
            .join("phoxal"))
    }
}

fn host_target() -> Result<String, Error> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|source| Error::CargoSpawn {
            operation: "read rustc host target".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(invalid(Path::new("rustc"), "cannot read the host target"));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| invalid(Path::new("rustc"), "rustc did not report a host target"))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn invalid(path: &Path, message: impl Into<String>) -> Error {
    Error::ManifestPreparation {
        path: path.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_selection_with_package_path_remains_git() {
        let selected: Selection = serde_yaml::from_str(
            "package: acme-motion\nversion: '1.2.3'\nsource:\n  git: https://example.test/motion.git\n  rev: 0123456789abcdef0123456789abcdef01234567\n  path: services/motion\n",
        )
        .expect("Git selection");
        assert!(matches!(selected.source, Some(Source::Git(_))));
    }
}
