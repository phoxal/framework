//! Exact participant installation and prepared Protobuf source publication.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::cargo::CargoOptions;
use super::document::{RobotDocument, Source};
use super::file_lock::ExclusiveFileLock;
use super::selection::PackageSource;
use super::{Error, ProjectLayout};
use fs4::TryLockError;

const PHOXAL_INDEX: &str = "sparse+https://phoxal.github.io/registry/";

#[derive(Debug, Clone)]
pub(crate) struct InstalledSelection {
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) binary: String,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) package_id: String,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) manifest_path: Option<PathBuf>,
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
                SelectionRequest {
                    binary: selection.binary.as_deref(),
                    source: &selection.source,
                },
                options,
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
                    SelectionRequest {
                        binary: component.binary.as_deref(),
                        source: &component.source,
                    },
                    options,
                )?,
            );
        }
    }
    Ok(selected)
}

struct SelectionRequest<'a> {
    binary: Option<&'a str>,
    source: &'a Source,
}

fn selected_installation(
    layout: &ProjectLayout,
    home: &Path,
    target: &str,
    request: SelectionRequest<'_>,
    options: &CargoOptions,
) -> Result<InstalledSelection, Error> {
    let SelectionRequest { binary, source } = request;
    if let Source::Path(path) = source {
        return local_selection(layout, Path::new(path), binary, options);
    }
    let (package, authored_version, store, source) = match source {
        Source::Package(package) => {
            let registry = package.registry.as_deref().unwrap_or("phoxal");
            (
                package.name.as_str(),
                Some(package.version.as_str()),
                home.join("packages/registry")
                    .join(registry)
                    .join(&package.name)
                    .join(&package.version)
                    .join(target),
                PackageSource::Registry {
                    source: if registry == "phoxal" {
                        PHOXAL_INDEX
                    } else {
                        registry
                    }
                    .to_owned(),
                },
            )
        }
        Source::Git(git) => (
            git.name.as_str(),
            None,
            home.join("packages/git")
                .join(&git.name)
                .join(&git.rev)
                .join(target),
            PackageSource::Git {
                source: format!("git+{}#{}", git.url, git.rev),
            },
        ),
        Source::Path(_) => unreachable!("handled above"),
    };
    let binary = binary.unwrap_or(package);
    let executable = store.join("bin").join(binary);
    let source_root = store.join("source");
    if !executable.is_file()
        || !source_root.join("api").is_dir()
        || !store.join("package-id").is_file()
    {
        return Err(invalid(
            layout.robot_manifest(),
            format!("{package} is not prepared; run `cargo phoxal prepare`"),
        ));
    }
    let version = if let Some(version) = authored_version {
        version.to_owned()
    } else {
        fs::read_to_string(store.join("package-version")).map_err(|source| Error::ArtifactFile {
            path: store.join("package-version"),
            source,
        })?
    };
    Ok(InstalledSelection {
        package: package.to_owned(),
        version,
        binary: binary.to_owned(),
        executable: Some(executable),
        package_id: fs::read_to_string(store.join("package-id")).map_err(|source| {
            Error::ArtifactFile {
                path: store.join("package-id"),
                source,
            }
        })?,
        source_path: None,
        manifest_path: None,
        source_root,
        source,
    })
}

fn local_selection(
    layout: &ProjectLayout,
    path: &Path,
    binary: Option<&str>,
    options: &CargoOptions,
) -> Result<InstalledSelection, Error> {
    if path.as_os_str().is_empty() || !path.is_relative() {
        return Err(invalid(
            layout.robot_manifest(),
            "local source must be a nonempty relative path",
        ));
    }
    let source_root =
        layout
            .root()
            .join(path)
            .canonicalize()
            .map_err(|source| Error::ArtifactFile {
                path: layout.root().join(path),
                source,
            })?;
    let manifest_path = source_root.join("Cargo.toml");
    let mut local_options = options.clone();
    local_options.features.clear();
    local_options.all_features = false;
    local_options.no_default_features = false;
    local_options.cargo_args.clear();
    let metadata =
        super::cargo::load_metadata_at(&manifest_path, &source_root, None, &local_options)?;
    let selected = metadata
        .packages
        .iter()
        .find(|candidate| candidate.manifest_path.as_std_path() == manifest_path)
        .ok_or_else(|| invalid(&manifest_path, "local participant is not a Cargo package"))?;
    let binary = binary.unwrap_or(&selected.name);
    let target = selected
        .targets
        .iter()
        .find(|target| target.name == binary && target.is_bin())
        .ok_or_else(|| {
            invalid(
                &manifest_path,
                format!("local participant has no binary `{binary}`"),
            )
        })?;
    Ok(InstalledSelection {
        package: selected.name.to_string(),
        version: selected.version.to_string(),
        binary: binary.to_owned(),
        executable: None,
        package_id: selected.id.to_string(),
        source_path: Some(target.src_path.as_std_path().to_owned()),
        manifest_path: Some(manifest_path.clone()),
        source_root,
        source: PackageSource::Local { manifest_path },
    })
}

/// Prepares the executable and API closure declared by `robot.yaml`.
pub(crate) fn prepare(
    layout: &ProjectLayout,
    options: &CargoOptions,
) -> Result<Vec<String>, Error> {
    options.validate()?;
    let text = fs::read_to_string(layout.robot_manifest()).map_err(|source| Error::ReadRobot {
        path: layout.robot_manifest().to_owned(),
        source,
    })?;
    let robot = super::document::parse_and_validate(&text, layout.robot_manifest())?;
    prepare_robot(layout, options, robot)
}

fn prepare_robot(
    layout: &ProjectLayout,
    options: &CargoOptions,
    robot: RobotDocument,
) -> Result<Vec<String>, Error> {
    let _lock = preparation_lock(layout)?;
    let home = phoxal_home()?;
    let _installation_lock = installation_lock(&home)?;
    let target = match &options.target {
        Some(target) => target.clone(),
        None => host_target()?,
    };
    let RobotDocument::V0 {
        robot, services, ..
    } = robot;
    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    for (instance, selection) in services {
        if seen.insert(format!("{:?}:{:?}", selection.source, selection.binary))
            && let Some(change) = prepare_selection(
                layout,
                options,
                &home,
                &target,
                &instance,
                &selection.source,
                selection.binary.as_deref(),
            )?
        {
            changes.push(change);
        }
    }
    for (instance, component) in robot.components {
        if component.driver.is_some()
            && seen.insert(format!("{:?}:{:?}", component.source, component.binary))
            && let Some(change) = prepare_selection(
                layout,
                options,
                &home,
                &target,
                &instance,
                &component.source,
                component.binary.as_deref(),
            )?
        {
            changes.push(change);
        }
    }
    Ok(changes)
}

fn prepare_selection(
    layout: &ProjectLayout,
    options: &CargoOptions,
    home: &Path,
    target: &str,
    instance: &str,
    source: &Source,
    binary: Option<&str>,
) -> Result<Option<String>, Error> {
    let manifest = layout.robot_manifest();
    if !identifier(instance) || binary.is_some_and(|binary| !identifier(binary)) {
        return Err(invalid(
            manifest,
            format!("{instance} has an invalid participant or binary name"),
        ));
    }
    if let Source::Path(path) = source {
        let path = Path::new(path);
        if path.as_os_str().is_empty() || !path.is_relative() {
            return Err(invalid(
                manifest,
                format!("{instance} local source must be a nonempty relative path"),
            ));
        }
        let api_source = layout.root().join(path).join("api");
        let service_source = layout.root().join(path).join("service.yaml");
        let component_source = layout.root().join(path).join("component.yaml");
        if !api_source.is_dir() && !service_source.is_file() && !component_source.is_file() {
            return Err(invalid(
                &api_source,
                format!(
                    "{instance} has no api/ directory or endpoint declaration (service.yaml or component.yaml)"
                ),
            ));
        }
        local_selection(layout, path, binary, options)?;
        return Ok(None);
    }
    let (package, expected_version, store, prepared) = match source {
        Source::Package(package) => {
            let registry = package.registry.as_deref().unwrap_or("phoxal");
            (
                package.name.as_str(),
                Some(package.version.as_str()),
                home.join("packages/registry")
                    .join(registry)
                    .join(&package.name)
                    .join(&package.version)
                    .join(target),
                layout
                    .root()
                    .join(".phoxal/registry")
                    .join(registry)
                    .join(&package.name)
                    .join(&package.version),
            )
        }
        Source::Git(git) => (
            git.name.as_str(),
            None,
            home.join("packages/git")
                .join(&git.name)
                .join(&git.rev)
                .join(target),
            layout
                .root()
                .join(".phoxal/git")
                .join(&git.name)
                .join(&git.rev),
        ),
        Source::Path(_) => unreachable!("handled above"),
    };
    let binary = binary.unwrap_or(package);
    let installed = store.join("bin").join(binary);
    // A built-in-only manifest contract retains service.yaml without an
    // api/ directory; either layout completes an installation.
    let complete_install = installed.is_file()
        && store.join(".crates.toml").is_file()
        && (store.join("source/api").is_dir() || store.join("source/service.yaml").is_file())
        && store.join("package-id").is_file()
        && store.join("package-version").is_file();
    if complete_install
        && (prepared.join("api").is_dir() || prepared.join("service.yaml").is_file())
    {
        let installed_manifest = store.join("source/service.yaml");
        let prepared_manifest = prepared.join("service.yaml");
        let manifest_agrees = match (installed_manifest.is_file(), prepared_manifest.is_file()) {
            (false, false) => true,
            (true, true) => same_file(&installed_manifest, &prepared_manifest),
            _ => false,
        };
        if !manifest_agrees {
            return Err(invalid(
                &prepared,
                format!(
                    "prepared contract inputs for {instance} are incomplete; remove this directory and rerun `cargo phoxal prepare`"
                ),
            ));
        }
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
    match source {
        Source::Package(package) => {
            command.args([
                "--registry",
                package.registry.as_deref().unwrap_or("phoxal"),
            ]);
            command.args([
                "--version",
                &format!("={}", package.version),
                package.name.as_str(),
            ]);
        }
        Source::Git(git) => {
            command.args(["--git", &git.url, "--rev", &git.rev, &git.name]);
        }
        Source::Path(_) => unreachable!("handled above"),
    }
    let operation = format!("install {package}");
    let output = command.output().map_err(|source| Error::CargoSpawn {
        operation: operation.clone(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::CargoCommand {
            operation,
            status: output
                .status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let (source_root, package_id, version) =
        captured_source(&output.stdout, binary, package, expected_version, options)?;
    for (name, value) in [
        ("package-id", package_id.as_str()),
        ("package-version", version.as_str()),
    ] {
        let path = install.path().join(name);
        fs::write(&path, value).map_err(|source| Error::ArtifactFile { path, source })?;
    }
    if let Source::Git(git) = source
        && let Some(path) = &git.path
        && !source_root.ends_with(path)
    {
        return Err(invalid(
            &source_root,
            format!("Git package {package} is not at selected path {path}"),
        ));
    }
    let api_source = source_root.join("api");
    // A built-in-only contract ships `service.yaml` without an `api/`
    // directory; both layouts constitute a runnable contract input set.
    if !api_source.is_dir() && !source_root.join("service.yaml").is_file() {
        return Err(invalid(
            &api_source,
            format!("{package} has no packaged api/ directory or service.yaml declaration"),
        ));
    }
    let validation = tempfile::tempdir_in(&staging).map_err(|source| Error::ArtifactFile {
        path: staging.clone(),
        source,
    })?;
    phoxal_build::validate_participant_api(&api_source, validation.path())
        .map_err(|error| invalid(&api_source, format!("invalid participant API: {error}")))?;
    retain_package_files(&source_root, install.path())?;
    if !install.path().join("bin").join(binary).is_file() {
        return Err(invalid(
            install.path(),
            format!("Cargo did not install binary `{binary}`"),
        ));
    }
    publish_api(&source_root, &prepared)?;
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
                    invalid(&store, format!("cannot publish installation: {source}; cannot restore previous installation: {restore}"))
                })?;
                return Err(Error::ArtifactFile {
                    path: store,
                    source,
                });
            }
            return Ok(Some(format!("{instance} {package} {version}")));
        }
    }
    fs::rename(install.path(), &store).map_err(|source| Error::ArtifactFile {
        path: store.clone(),
        source,
    })?;
    Ok(Some(format!("{instance} {package} {version}")))
}

fn captured_source(
    output: &[u8],
    binary: &str,
    package: &str,
    expected_version: Option<&str>,
    options: &CargoOptions,
) -> Result<(PathBuf, String, String), Error> {
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
        let Some(reported_id) = value["package_id"].as_str() else {
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
            if document["package"]["name"].as_str() == Some(package) {
                let mut command = cargo_metadata::MetadataCommand::new();
                command.cargo_path(options.cargo_program());
                command
                    .manifest_path(&manifest)
                    .current_dir(directory)
                    .no_deps();
                if options.offline || matches!(options.lock, super::cargo::LockMode::Frozen) {
                    command.other_options(vec!["--offline".to_owned()]);
                }
                let metadata = command.exec().map_err(|error| {
                    invalid(
                        &manifest,
                        format!("cannot read Cargo package information: {error}"),
                    )
                })?;
                if let Some(candidate) = metadata.packages.iter().find(|candidate| {
                    candidate.manifest_path.as_std_path() == manifest
                        && candidate.name == package
                        && expected_version
                            .is_none_or(|version| candidate.version.to_string() == version)
                }) {
                    return Ok((
                        directory.to_owned(),
                        reported_id.to_owned(),
                        candidate.version.to_string(),
                    ));
                }
            }
        }
    }
    Err(invalid(
        Path::new("Cargo.toml"),
        format!(
            "Cargo did not report a binary artifact for package {package} {expected_version:?} ({binary})"
        ),
    ))
}

fn publish_api(source_root: &Path, destination_root: &Path) -> Result<(), Error> {
    let api_source = source_root.join("api");
    if let Some(parent) = destination_root.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::ArtifactFile {
            path: parent.to_owned(),
            source,
        })?;
        let staging = tempfile::tempdir_in(parent).map_err(|source| Error::ArtifactFile {
            path: parent.to_owned(),
            source,
        })?;
        let candidate = staging.path();
        // A built-in-only manifest contract has no api/ directory to copy;
        // the prepared tree keeps the api/ layout shape as an empty directory
        // beside the manifest so consumers never see a partial publication.
        if api_source.is_dir() {
            copy_protos(&api_source, &candidate.join("api"))?;
        } else {
            fs::create_dir_all(candidate.join("api")).map_err(|source| Error::ArtifactFile {
                path: candidate.join("api"),
                source,
            })?;
        }
        let manifest = source_root.join("service.yaml");
        let manifest_destination = candidate.join("service.yaml");
        if manifest.is_file() {
            fs::copy(&manifest, &manifest_destination).map_err(|source| Error::ArtifactFile {
                path: manifest_destination.clone(),
                source,
            })?;
        }
        if destination_root.exists() {
            if same_tree(candidate, destination_root)? {
                return Ok(());
            }
            return Err(invalid(
                destination_root,
                "prepared source differs for the same exact package; remove the corrupted directory before retrying",
            ));
        }
        fs::rename(candidate, destination_root).map_err(|source| Error::ArtifactFile {
            path: destination_root.to_owned(),
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

fn same_file(left: &Path, right: &Path) -> bool {
    fs::read(left).is_ok_and(|left| fs::read(right).is_ok_and(|right| left == right))
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

fn retain_package_files(source: &Path, installed: &Path) -> Result<(), Error> {
    let retained = installed.join("source");
    // A manifest-only package has no api/ to retain, but the retained
    // source/ root must still exist for the manifest copy below.
    fs::create_dir_all(&retained).map_err(|error| Error::ArtifactFile {
        path: retained.clone(),
        source: error,
    })?;
    copy_package_source(&source.join("api"), &retained.join("api"))?;
    let service = source.join("service.yaml");
    if service.is_file() {
        fs::copy(&service, retained.join("service.yaml")).map_err(|error| Error::ArtifactFile {
            path: service,
            source: error,
        })?;
    }
    let component = source.join("component.yaml");
    if component.is_file() {
        fs::copy(&component, retained.join("component.yaml")).map_err(|error| {
            Error::ArtifactFile {
                path: component,
                source: error,
            }
        })?;
        super::bundle::retain_component_resources(source, &retained)?;
    }
    Ok(())
}

fn copy_package_source(source: &Path, retained: &Path) -> Result<(), Error> {
    // A built-in-only manifest contract has no api/ directory to retain.
    if !source.exists() {
        return Ok(());
    }
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
        let selected: Source = serde_yaml::from_str(
            "git:\n  name: acme-motion\n  url: https://example.test/motion.git\n  rev: 0123456789abcdef0123456789abcdef01234567\n  path: services/motion\n",
        )
        .expect("Git selection");
        assert!(matches!(selected, Source::Git(_)));
    }

    #[test]
    fn captured_source_accepts_workspace_inherited_package_version() {
        let directory = tempfile::tempdir().expect("temporary workspace");
        let root = directory.path();
        let provider = root.join("provider");
        fs::create_dir_all(provider.join("src")).expect("provider source directory");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"provider\"]\n[workspace.package]\nversion = \"1.2.3\"\n",
        )
        .expect("workspace manifest");
        fs::write(
            provider.join("Cargo.toml"),
            "[package]\nname = \"proof-provider\"\nversion.workspace = true\nedition = \"2024\"\n",
        )
        .expect("provider manifest");
        let source = provider.join("src/main.rs");
        fs::write(&source, "fn main() {}\n").expect("provider source");
        let message = serde_json::json!({"reason":"compiler-artifact","package_id":"path+file:///proof-provider#1.2.3","target":{"name":"proof-provider","kind":["bin"],"src_path":source}}).to_string();
        let options = CargoOptions {
            offline: true,
            ..CargoOptions::default()
        };
        let (captured, package_id, version) = captured_source(
            message.as_bytes(),
            "proof-provider",
            "proof-provider",
            Some("1.2.3"),
            &options,
        )
        .expect("inherited version");
        assert_eq!(captured, provider);
        assert_eq!(package_id, "path+file:///proof-provider#1.2.3");
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn retained_component_contains_runtime_resources_without_package_source() {
        let directory = tempfile::tempdir().expect("temporary package");
        let source = directory.path().join("source");
        let installed = directory.path().join("installed");
        fs::create_dir_all(source.join("api")).expect("API directory");
        fs::create_dir_all(source.join("assets")).expect("asset directory");
        fs::create_dir_all(source.join("src")).expect("source directory");
        fs::write(source.join("api/component.proto"), "syntax = \"proto3\";").expect("API file");
        fs::write(source.join("component.yaml"), "schema: phoxal/component/v0\nmodel: { file: model.xml, root_body: mount }\ncapabilities: {}\n").expect("component definition");
        fs::write(source.join("model.xml"), "<mujoco model=\"proof\"/>").expect("model");
        fs::write(source.join("assets/mesh.obj"), "proof mesh").expect("resource");
        fs::write(source.join("src/main.rs"), "fn main() {}").expect("Rust source");
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"proof\"\nversion = \"1.0.0\"\n",
        )
        .expect("manifest");

        retain_package_files(&source, &installed).expect("retain runtime resources");
        for path in [
            "api/component.proto",
            "component.yaml",
            "model.xml",
            "assets/mesh.obj",
        ] {
            assert!(
                installed.join("source").join(path).is_file(),
                "missing {path}"
            );
        }
        assert!(!installed.join("source/Cargo.toml").exists());
        assert!(!installed.join("source/src").exists());
    }
}
