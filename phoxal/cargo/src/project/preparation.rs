//! Atomic preparation of the authored Cargo graph.
//!
//! The project compiler owns only the small set of infrastructure defaults
//! that are part of every hardware execution.  They are ordinary dependency
//! entries in the robot's Cargo manifest, so Cargo remains the one resolver and
//! the resulting lockfile remains inspectable by users and editors.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use cargo_metadata::MetadataCommand;
use fs4::TryLockError;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::project::ProjectLayout;
use crate::project::cargo::{CargoOptions, LockMode};
use crate::project::document::RobotDocument;
use crate::project::error::Error;
use crate::project::file_lock::ExclusiveFileLock;
use crate::project::robot_api;

/// The official package key used by the mandatory supervisor dependency.
pub const SUPERVISOR_DEPENDENCY_KEY: &str = "phoxal-supervisor";
/// The unconstrained package version requirement used for a fresh project.
/// Existing requirements and locked selections always take precedence.
pub const SUPERVISOR_VERSION_REQUIREMENT: &str = "=0.0.0-dev.2";
/// The configured registry containing official Phoxal packages.
pub const SUPERVISOR_REGISTRY: &str = "phoxal";

/// One visible change made by preparation.
///
/// Ordinary unlocked preparation adds the mandatory supervisor dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparationChange {
    /// Mandatory supervisor dependency added to `[dependencies]`.
    SupervisorDependencyAdded {
        /// Exact root manifest dependency key added by the tool.
        dependency: String,
        /// Human-readable Cargo requirement written for the dependency.
        requirement: String,
    },
    /// Stable generated robot API dependency added to the root manifest.
    RobotApiDependencyAdded { package: String, path: String },
    /// One tool-owned robot API source file changed.
    RobotApiFileWritten { path: String },
    /// One local service's generated contract artifact changed.
    ServiceContractFileWritten { package: String, path: String },
}

#[derive(Debug, Clone)]
struct LockSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

/// The manifest and candidate workspace locks captured before preparation.
///
/// Cargo metadata may update a workspace lock after the manifest has been
/// changed.  Keeping this transaction alive through source selection lets a
/// failed addition restore both authored inputs and lock state before the
/// caller observes the error.
#[derive(Debug)]
pub(crate) struct ManifestTransaction {
    _lock: ExclusiveFileLock,
    manifest: PathBuf,
    original_manifest: Vec<u8>,
    locks: Vec<LockSnapshot>,
    managed_files: Vec<LockSnapshot>,
    changes: Vec<PreparationChange>,
    /// `true` once `commit()` has run. Drop will skip rollback when
    /// this is set so a successful commit is not undone by an
    /// incidental drop later.
    committed: bool,
}

impl Drop for ManifestTransaction {
    fn drop(&mut self) {
        // If the transaction was never committed, every error path
        // that propagates with `?` will drop the transaction here.
        // Restore the original manifest bytes (or any captured lock
        // file snapshot) so a partly-failed preparation never leaves
        // the workspace modified.
        if self.committed {
            return;
        }
        // Best-effort restore; we cannot return an error from drop,
        // so log to stderr and leave the workspace in a recoverable
        // state for the operator.
        if !self.changes.is_empty()
            && let Err(source) = atomic_write(&self.manifest, &self.original_manifest)
        {
            eprintln!(
                "cargo-phoxal: failed to restore {} after preparation error: {source}",
                self.manifest.display()
            );
            return;
        }
        for snapshot in &self.locks {
            let outcome = match &snapshot.contents {
                Some(contents) => match fs::read(&snapshot.path) {
                    Ok(current) if current == *contents => Ok(()),
                    Ok(_) => atomic_write(&snapshot.path, contents),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        atomic_write(&snapshot.path, contents)
                    }
                    Err(_) => Ok(()),
                },
                None => match fs::remove_file(&snapshot.path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(source) => Err(source),
                },
            };
            if let Err(source) = outcome {
                eprintln!(
                    "cargo-phoxal: failed to restore {} after preparation error: {source}",
                    snapshot.path.display()
                );
            }
        }
        for snapshot in &self.managed_files {
            if let Err(source) = restore_snapshot(snapshot) {
                eprintln!(
                    "cargo-phoxal: failed to restore {} after preparation error: {source}",
                    snapshot.path.display()
                );
            }
        }
    }
}

impl ManifestTransaction {
    /// Finish a successful preparation and release its source mutation lock.
    pub(crate) fn commit(mut self) -> Vec<PreparationChange> {
        self.committed = true;
        std::mem::take(&mut self.changes)
    }

    /// Restores all files captured before an unsuccessful preparation.
    pub(crate) fn rollback(&self) -> Result<(), Error> {
        if !self.changes.is_empty() {
            atomic_write(&self.manifest, &self.original_manifest).map_err(|source| {
                Error::ManifestRestore {
                    path: self.manifest.clone(),
                    source,
                }
            })?;
        }
        for snapshot in &self.locks {
            match &snapshot.contents {
                Some(contents) => match fs::read(&snapshot.path) {
                    Ok(current) if current == *contents => {}
                    Ok(_) => {
                        atomic_write(&snapshot.path, contents).map_err(|source| {
                            Error::ManifestRestore {
                                path: snapshot.path.clone(),
                                source,
                            }
                        })?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        atomic_write(&snapshot.path, contents).map_err(|source| {
                            Error::ManifestRestore {
                                path: snapshot.path.clone(),
                                source,
                            }
                        })?;
                    }
                    Err(source) => {
                        return Err(Error::ManifestRestore {
                            path: snapshot.path.clone(),
                            source,
                        });
                    }
                },
                None => match fs::remove_file(&snapshot.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(Error::ManifestRestore {
                            path: snapshot.path.clone(),
                            source,
                        });
                    }
                },
            }
        }
        for snapshot in &self.managed_files {
            restore_snapshot(snapshot).map_err(|source| Error::ManifestRestore {
                path: snapshot.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    fn snapshot_managed_file(&mut self, path: &Path) -> Result<(), Error> {
        if self.managed_files.iter().any(|entry| entry.path == path) {
            return Ok(());
        }
        let contents = match fs::read(path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(Error::ReadManifest {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        self.managed_files.push(LockSnapshot {
            path: path.to_owned(),
            contents,
        });
        Ok(())
    }
}

fn restore_snapshot(snapshot: &LockSnapshot) -> Result<(), std::io::Error> {
    match &snapshot.contents {
        Some(contents) => atomic_write(&snapshot.path, contents),
        None => match fs::remove_file(&snapshot.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
    }
}

/// Ensure mandatory infrastructure is present in the authored root graph.
///
/// Locked and frozen modes inspect the need before any write and return an
/// initialization diagnostic.  Ordinary mode writes one atomic manifest and
/// leaves the resulting lock update to Cargo metadata.
pub(crate) fn ensure_required_dependencies(
    layout: &ProjectLayout,
    options: &CargoOptions,
) -> Result<ManifestTransaction, Error> {
    options.validate()?;
    let manifest = layout.cargo_manifest().to_owned();
    let (lock, workspace_root) = acquire_preparation_lock(layout, &manifest)?;
    let original_manifest = fs::read(&manifest).map_err(|source| Error::ReadManifest {
        path: manifest.clone(),
        source,
    })?;
    let mut document = String::from_utf8(original_manifest.clone())
        .map_err(|error| Error::ManifestPreparation {
            path: manifest.clone(),
            message: format!("Cargo.toml is not UTF-8: {error}"),
        })?
        .parse::<DocumentMut>()
        .map_err(|error| Error::ManifestPreparation {
            path: manifest.clone(),
            message: format!("Cargo.toml is not valid TOML: {error}"),
        })?;
    let locks = lock_snapshots(&workspace_root)?;

    let has_supervisor = document
        .get("dependencies")
        .and_then(Item::as_table)
        .is_some_and(|dependencies| dependencies.contains_key(SUPERVISOR_DEPENDENCY_KEY));
    if has_supervisor {
        return Ok(ManifestTransaction {
            _lock: lock,
            manifest,
            original_manifest,
            locks,
            managed_files: Vec::new(),
            changes: Vec::new(),
            committed: false,
        });
    }

    let lock_mode = match options.lock {
        LockMode::Locked => Some("--locked"),
        LockMode::Frozen => Some("--frozen"),
        LockMode::Unlocked => None,
    };
    if let Some(lock_mode) = lock_mode {
        return Err(Error::MissingInitialization {
            path: manifest,
            dependency: SUPERVISOR_DEPENDENCY_KEY.to_owned(),
            lock_mode,
        });
    }

    let dependencies = match document.get_mut("dependencies") {
        Some(item) if item.is_table() => {
            item.as_table_mut()
                .ok_or_else(|| Error::ManifestPreparation {
                    path: manifest.clone(),
                    message: "[dependencies] is not a standard TOML table".to_owned(),
                })?
        }
        Some(_) => {
            return Err(Error::ManifestPreparation {
                path: manifest,
                message: "[dependencies] must be a TOML table".to_owned(),
            });
        }
        None => {
            document["dependencies"] = Item::Table(Table::new());
            document
                .get_mut("dependencies")
                .and_then(Item::as_table_mut)
                .ok_or_else(|| Error::ManifestPreparation {
                    path: manifest.clone(),
                    message: "could not create [dependencies] table".to_owned(),
                })?
        }
    };
    let mut requirement = InlineTable::new();
    requirement.insert("version", Value::from(SUPERVISOR_VERSION_REQUIREMENT));
    requirement.insert("registry", Value::from(SUPERVISOR_REGISTRY));
    dependencies.insert(
        SUPERVISOR_DEPENDENCY_KEY,
        Item::Value(Value::InlineTable(requirement)),
    );
    let prepared = document.to_string().into_bytes();
    if let Err(source) = atomic_write(&manifest, &prepared) {
        // `atomic_write` may have persisted the replacement before a final
        // parent-directory sync reports an error.  Restore the original bytes
        // even on that path so a failed initialization never leaves a partial
        // authored graph behind.
        if let Err(restore) = atomic_write(&manifest, &original_manifest) {
            return Err(Error::ManifestRestore {
                path: manifest,
                source: restore,
            });
        }
        return Err(Error::ManifestWrite {
            path: manifest,
            source,
        });
    }

    Ok(ManifestTransaction {
        _lock: lock,
        manifest,
        original_manifest,
        locks,
        managed_files: Vec::new(),
        changes: vec![PreparationChange::SupervisorDependencyAdded {
            dependency: SUPERVISOR_DEPENDENCY_KEY.to_owned(),
            requirement: format!(
                "version = \"{SUPERVISOR_VERSION_REQUIREMENT}\", registry = \"{SUPERVISOR_REGISTRY}\""
            ),
        }],
        committed: false,
    })
}

/// Installs the complete pre-metadata robot API candidate and its one stable
/// root dependency inside the existing project preparation transaction.
pub(crate) fn prepare_robot_api_in_transaction(
    layout: &ProjectLayout,
    document: &RobotDocument,
    options: &CargoOptions,
    transaction: &mut ManifestTransaction,
) -> Result<(), Error> {
    let RobotDocument::V0 { services, .. } = document;
    if services.is_empty() {
        return Ok(());
    }
    let service_changes = prepare_local_service_contracts(document, layout.root(), options)?;
    transaction.changes.extend(service_changes);
    let candidate = robot_api::candidate(document, layout.root())?;
    let generated_root = layout.root().join(robot_api::DIRECTORY);
    let files = [
        (
            generated_root.join("Cargo.toml"),
            candidate.manifest.as_bytes(),
        ),
        (generated_root.join("src/lib.rs"), candidate.lib.as_bytes()),
        (
            generated_root.join("src/contracts.rs"),
            candidate.contracts.as_bytes(),
        ),
        (
            generated_root.join("src/services.rs"),
            candidate.services.as_bytes(),
        ),
    ];

    let manifest_text =
        fs::read_to_string(layout.cargo_manifest()).map_err(|source| Error::ReadManifest {
            path: layout.cargo_manifest().to_owned(),
            source,
        })?;
    let mut manifest =
        manifest_text
            .parse::<DocumentMut>()
            .map_err(|error| Error::ManifestPreparation {
                path: layout.cargo_manifest().to_owned(),
                message: format!("Cargo.toml is not valid TOML: {error}"),
            })?;
    let expected_dependency = robot_api_dependency(&candidate.package);
    let dependency_current = manifest
        .get("dependencies")
        .and_then(Item::as_table)
        .and_then(|dependencies| dependencies.get(robot_api::DEPENDENCY_KEY));
    let dependency_matches = dependency_current
        .is_some_and(|current| robot_api_dependency_matches(current, &candidate.package));
    let manifest_matches = fs::read(&files[0].0)
        .ok()
        .is_some_and(|contents| contents == files[0].1);
    let sources_exist = files[1..].iter().all(|(path, _)| path.is_file());
    if dependency_matches && manifest_matches && sources_exist {
        return Ok(());
    }

    if let Some(lock_mode) = match options.lock {
        LockMode::Locked => Some("--locked"),
        LockMode::Frozen => Some("--frozen"),
        LockMode::Unlocked => None,
    } {
        return Err(Error::MissingInitialization {
            path: layout.cargo_manifest().to_owned(),
            dependency: robot_api::DEPENDENCY_KEY.to_owned(),
            lock_mode,
        });
    }

    if !dependency_matches {
        if manifest.get("dependencies").is_none() {
            manifest["dependencies"] = Item::Table(Table::new());
        }
        let dependencies =
            manifest["dependencies"]
                .as_table_mut()
                .ok_or_else(|| Error::ManifestPreparation {
                    path: layout.cargo_manifest().to_owned(),
                    message: "[dependencies] must be a standard TOML table".to_owned(),
                })?;
        dependencies.insert(robot_api::DEPENDENCY_KEY, expected_dependency);
        atomic_write(layout.cargo_manifest(), manifest.to_string().as_bytes()).map_err(
            |source| Error::ManifestWrite {
                path: layout.cargo_manifest().to_owned(),
                source,
            },
        )?;
        transaction
            .changes
            .push(PreparationChange::RobotApiDependencyAdded {
                package: candidate.package,
                path: robot_api::DIRECTORY.to_owned(),
            });
    }

    for (path, contents) in files {
        if fs::read(&path).ok().as_deref() == Some(contents) {
            continue;
        }
        transaction.snapshot_managed_file(&path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::ManifestWrite {
                path: path.clone(),
                source,
            })?;
        }
        atomic_write(&path, contents).map_err(|source| Error::ManifestWrite {
            path: path.clone(),
            source,
        })?;
        let relative = path
            .strip_prefix(layout.root())
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        transaction
            .changes
            .push(PreparationChange::RobotApiFileWritten { path: relative });
    }
    Ok(())
}

#[derive(Debug)]
struct LocalContractPackage {
    name: String,
    root: PathBuf,
    protos: Vec<PathBuf>,
    includes: Vec<PathBuf>,
    generated: PathBuf,
    dependencies: Vec<ContractDependency>,
}

#[derive(Debug, Clone)]
struct ContractDependency {
    dependency: String,
    proto_package: String,
    rust_path: String,
}

#[derive(Debug)]
struct ResolvedContractDependency {
    declaration: ContractDependency,
    package: String,
    root: PathBuf,
    local: bool,
}

#[derive(Debug)]
struct ContractPreparation {
    package: LocalContractPackage,
    dependencies: Vec<ResolvedContractDependency>,
}

fn prepare_local_service_contracts(
    document: &RobotDocument,
    robot_root: &Path,
    options: &CargoOptions,
) -> Result<Vec<PreparationChange>, Error> {
    let RobotDocument::V0 { services, .. } = document;
    let mut roots = services
        .values()
        .filter_map(|selection| match &selection.source {
            Some(crate::project::document::ServiceSource::Path(source)) => {
                Some(robot_root.join(&source.path))
            }
            _ => None,
        })
        .map(|path| {
            path.canonicalize()
                .map_err(|source| Error::ManifestPreparation {
                    path: path.clone(),
                    message: format!("cannot resolve local service source: {source}"),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    roots.sort();
    roots.dedup();
    let mut preparations = Vec::new();
    let mut visit_state = BTreeMap::new();
    for root in roots {
        collect_contract_preparations(&root, options, &mut visit_state, &mut preparations)?;
    }
    let mut packages = preparations
        .iter()
        .map(|preparation| preparation.package.root.clone())
        .collect::<Vec<_>>();
    packages.sort();
    packages.dedup();

    let mut locks = Vec::with_capacity(packages.len());
    for root in &packages {
        let lock_directory = root.join("target/phoxal");
        fs::create_dir_all(&lock_directory).map_err(|source| Error::ManifestPreparation {
            path: root.clone(),
            message: format!("cannot create local contract lock directory: {source}"),
        })?;
        let lock_path = lock_directory.join("contract-preparation.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| Error::ManifestPreparation {
                path: lock_path.clone(),
                message: format!("cannot open local contract lock: {source}"),
            })?;
        let lock =
            ExclusiveFileLock::try_acquire(file).map_err(|error| Error::ManifestPreparation {
                path: lock_path,
                message: match error {
                    TryLockError::WouldBlock => {
                        "another command is preparing this local service contract".to_owned()
                    }
                    TryLockError::Error(source) => {
                        format!("cannot lock local service contract: {source}")
                    }
                },
            })?;
        locks.push(lock);
    }

    let mut changes = Vec::new();
    for preparation in preparations {
        let package = preparation.package;
        let root = &package.root;
        let candidate_parent = root.join("target/phoxal");
        let candidate = tempfile::Builder::new()
            .prefix("contract-candidate-")
            .tempdir_in(&candidate_parent)
            .map_err(|source| Error::ManifestPreparation {
                path: candidate_parent,
                message: format!("cannot create contract candidate: {source}"),
            })?;
        let mut descriptor_bytes = BTreeMap::<String, Vec<u8>>::new();
        let mut extern_paths = Vec::with_capacity(preparation.dependencies.len());
        for dependency in &preparation.dependencies {
            let dependency_package = local_contract_package(&dependency.root)?;
            phoxal_build::verify_contract_metadata(
                &dependency_package.protos,
                &dependency_package.includes,
                &dependency_package.generated,
            )
            .map_err(|error| Error::ManifestPreparation {
                path: dependency.root.join("Cargo.toml"),
                message: format!(
                    "contract dependency `{}` is stale or invalid: {error}",
                    dependency.package
                ),
            })?;
            let descriptor_path = dependency_package.generated.join("phoxal-descriptors.bin");
            let descriptors =
                fs::read(&descriptor_path).map_err(|source| Error::ManifestPreparation {
                    path: descriptor_path.clone(),
                    message: format!("cannot read dependency contract descriptors: {source}"),
                })?;
            let pool =
                prost_reflect::DescriptorPool::decode(descriptors.as_slice()).map_err(|error| {
                    Error::ManifestPreparation {
                        path: descriptor_path,
                        message: format!("dependency contract descriptors are invalid: {error}"),
                    }
                })?;
            let expected = dependency.declaration.proto_package.trim_start_matches('.');
            if !pool.files().any(|file| file.package_name() == expected) {
                return Err(Error::ManifestPreparation {
                    path: dependency.root.join("Cargo.toml"),
                    message: format!(
                        "contract dependency `{}` does not provide Protobuf package `{}`",
                        dependency.package, dependency.declaration.proto_package
                    ),
                });
            }
            descriptor_bytes
                .entry(dependency.package.clone())
                .or_insert(descriptors);
            extern_paths.push((
                dependency.declaration.proto_package.as_str(),
                dependency.declaration.rust_path.as_str(),
            ));
        }
        let descriptors = descriptor_bytes
            .iter()
            .map(|(package, descriptors)| {
                phoxal_build::DependencyDescriptor::new(package, descriptors)
            })
            .collect::<Vec<_>>();
        phoxal_build::generate_contract_package(
            &package.protos,
            &package.includes,
            candidate.path(),
            &descriptors,
            &extern_paths,
        )
        .map_err(|error| Error::ManifestPreparation {
            path: root.join("Cargo.toml"),
            message: format!("local contract generation failed: {error}"),
        })?;
        phoxal_build::verify_contract_metadata(
            &package.protos,
            &package.includes,
            candidate.path(),
        )
        .map_err(|error| Error::ManifestPreparation {
            path: root.join("Cargo.toml"),
            message: format!("generated local contract candidate is invalid: {error}"),
        })?;
        install_contract_candidate(&package, candidate.path(), &mut changes)?;
    }
    drop(locks);
    Ok(changes)
}

fn collect_contract_preparations(
    root: &Path,
    options: &CargoOptions,
    visit_state: &mut BTreeMap<PathBuf, bool>,
    preparations: &mut Vec<ContractPreparation>,
) -> Result<(), Error> {
    let root = root
        .canonicalize()
        .map_err(|source| Error::ManifestPreparation {
            path: root.to_owned(),
            message: format!("cannot resolve contract package root: {source}"),
        })?;
    match visit_state.get(&root) {
        Some(true) => return Ok(()),
        Some(false) => {
            return Err(Error::ManifestPreparation {
                path: root.join("Cargo.toml"),
                message: "contract package dependencies contain a cycle".to_owned(),
            });
        }
        None => {}
    }
    visit_state.insert(root.clone(), false);
    let package = local_contract_package(&root)?;
    let dependencies = resolve_contract_dependencies(&package, options)?;
    for dependency in &dependencies {
        if dependency.root.starts_with(&root) && dependency.root == root {
            return Err(Error::ManifestPreparation {
                path: root.join("Cargo.toml"),
                message: format!(
                    "contract package `{}` depends on itself through `{}`",
                    package.name, dependency.declaration.dependency
                ),
            });
        }
        if dependency.local {
            collect_contract_preparations(&dependency.root, options, visit_state, preparations)?;
        }
    }
    visit_state.insert(root, true);
    preparations.push(ContractPreparation {
        package,
        dependencies,
    });
    Ok(())
}

fn resolve_contract_dependencies(
    package: &LocalContractPackage,
    options: &CargoOptions,
) -> Result<Vec<ResolvedContractDependency>, Error> {
    if package.dependencies.is_empty() {
        return Ok(Vec::new());
    }
    let metadata = contract_package_metadata(&package.root, options)?;
    let manifest = package.root.join("Cargo.toml");
    let canonical_manifest =
        manifest
            .canonicalize()
            .map_err(|source| Error::ManifestPreparation {
                path: manifest.clone(),
                message: format!("cannot resolve contract manifest: {source}"),
            })?;
    let owner = metadata
        .packages
        .iter()
        .find(|candidate| {
            PathBuf::from(candidate.manifest_path.as_std_path())
                .canonicalize()
                .is_ok_and(|path| path == canonical_manifest)
        })
        .ok_or_else(|| Error::ManifestPreparation {
            path: manifest.clone(),
            message: "cargo metadata did not return the contract package".to_owned(),
        })?;
    let node = metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == owner.id))
        .ok_or_else(|| Error::ManifestPreparation {
            path: manifest.clone(),
            message: "cargo metadata did not resolve the contract package".to_owned(),
        })?;

    package
        .dependencies
        .iter()
        .map(|declaration| {
            let normalized = declaration.dependency.replace('-', "_");
            let node_dependency = node
                .deps
                .iter()
                .find(|dependency| {
                    dependency.name == declaration.dependency || dependency.name == normalized
                })
                .ok_or_else(|| Error::ManifestPreparation {
                    path: manifest.clone(),
                    message: format!(
                        "contract dependency `{}` is not a resolved direct Cargo dependency",
                        declaration.dependency
                    ),
                })?;
            let resolved = metadata
                .packages
                .iter()
                .find(|candidate| candidate.id == node_dependency.pkg)
                .ok_or_else(|| Error::ManifestPreparation {
                    path: manifest.clone(),
                    message: format!(
                        "cargo metadata omitted resolved contract dependency `{}`",
                        declaration.dependency
                    ),
                })?;
            let root = PathBuf::from(resolved.manifest_path.as_std_path())
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| Error::ManifestPreparation {
                    path: PathBuf::from(resolved.manifest_path.as_std_path()),
                    message: "resolved dependency manifest has no package directory".to_owned(),
                })?;
            Ok(ResolvedContractDependency {
                declaration: declaration.clone(),
                package: resolved.name.to_string(),
                root,
                local: resolved.source.is_none(),
            })
        })
        .collect()
}

fn contract_package_metadata(
    root: &Path,
    options: &CargoOptions,
) -> Result<cargo_metadata::Metadata, Error> {
    let manifest = root.join("Cargo.toml");
    let mut command = MetadataCommand::new();
    command
        .cargo_path(options.cargo_program())
        .manifest_path(&manifest)
        .current_dir(root);
    let mut extra = options
        .lock
        .flags()
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect::<Vec<_>>();
    if options.offline {
        extra.push("--offline".to_owned());
    }
    command.other_options(extra);
    command.exec().map_err(|error| Error::ManifestPreparation {
        path: manifest,
        message: format!("cannot resolve contract dependencies with Cargo: {error}"),
    })
}

fn install_contract_candidate(
    package: &LocalContractPackage,
    candidate: &Path,
    changes: &mut Vec<PreparationChange>,
) -> Result<(), Error> {
    let candidate_files = read_flat_directory(candidate, "generated contract candidate")?;
    let accepted_files = if package.generated.is_dir() {
        read_flat_directory(&package.generated, "accepted generated contract")?
    } else {
        BTreeMap::new()
    };
    let changed_names = candidate_files
        .iter()
        .filter(|(name, contents)| accepted_files.get(*name) != Some(*contents))
        .map(|(name, _)| name.clone())
        .chain(
            accepted_files
                .keys()
                .filter(|name| !candidate_files.contains_key(*name))
                .cloned(),
        )
        .collect::<Vec<_>>();
    if changed_names.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(&package.generated).map_err(|source| Error::ManifestPreparation {
        path: package.generated.clone(),
        message: format!("cannot create generated contract directory: {source}"),
    })?;
    let snapshots = changed_names
        .iter()
        .map(|name| LockSnapshot {
            path: package.generated.join(name),
            contents: accepted_files.get(name).cloned(),
        })
        .collect::<Vec<_>>();
    let install = (|| -> Result<(), std::io::Error> {
        for (name, contents) in &candidate_files {
            if accepted_files.get(name) != Some(contents) {
                atomic_write(&package.generated.join(name), contents)?;
            }
        }
        for name in accepted_files.keys() {
            if !candidate_files.contains_key(name) {
                fs::remove_file(package.generated.join(name))?;
            }
        }
        Ok(())
    })();
    if let Err(source) = install {
        for snapshot in &snapshots {
            if let Err(restore) = restore_snapshot(snapshot) {
                return Err(Error::ManifestPreparation {
                    path: snapshot.path.clone(),
                    message: format!(
                        "contract installation failed ({source}) and rollback failed: {restore}"
                    ),
                });
            }
        }
        return Err(Error::ManifestPreparation {
            path: package.generated.clone(),
            message: format!("cannot install complete generated contract candidate: {source}"),
        });
    }

    changes.extend(changed_names.into_iter().map(|name| {
        let destination = package.generated.join(name);
        PreparationChange::ServiceContractFileWritten {
            package: package.name.clone(),
            path: destination
                .strip_prefix(&package.root)
                .unwrap_or(&destination)
                .to_string_lossy()
                .into_owned(),
        }
    }));
    Ok(())
}

fn read_flat_directory(
    directory: &Path,
    description: &str,
) -> Result<BTreeMap<OsString, Vec<u8>>, Error> {
    let entries = fs::read_dir(directory)
        .map_err(|source| Error::ManifestPreparation {
            path: directory.to_owned(),
            message: format!("cannot inspect {description}: {source}"),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| Error::ManifestPreparation {
            path: directory.to_owned(),
            message: format!("cannot inspect {description}: {source}"),
        })?;
    let mut files = BTreeMap::new();
    for entry in entries {
        if !entry
            .file_type()
            .map_err(|source| Error::ManifestPreparation {
                path: entry.path(),
                message: format!("cannot inspect {description} entry: {source}"),
            })?
            .is_file()
        {
            return Err(Error::ManifestPreparation {
                path: entry.path(),
                message: format!("{description} contains a non-file entry"),
            });
        }
        let contents = fs::read(entry.path()).map_err(|source| Error::ManifestPreparation {
            path: entry.path(),
            message: format!("cannot read {description}: {source}"),
        })?;
        files.insert(entry.file_name(), contents);
    }
    Ok(files)
}

fn local_contract_package(root: &Path) -> Result<LocalContractPackage, Error> {
    let manifest = root.join("Cargo.toml");
    let text = fs::read_to_string(&manifest).map_err(|source| Error::ReadManifest {
        path: manifest.clone(),
        source,
    })?;
    let value = toml::from_str::<toml::Value>(&text).map_err(|source| Error::ParseManifest {
        path: manifest.clone(),
        source,
    })?;
    let name = value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| Error::ManifestPreparation {
            path: manifest.clone(),
            message: "local service package has no package.name".to_owned(),
        })?
        .to_owned();
    let contract = value
        .get("package")
        .and_then(|package| package.get("metadata"))
        .and_then(|metadata| metadata.get("phoxal"))
        .and_then(|phoxal| phoxal.get("contract"))
        .ok_or_else(|| Error::ManifestPreparation {
            path: manifest.clone(),
            message: "selected local service has no package.metadata.phoxal.contract".to_owned(),
        })?;
    let protos = contract_paths(contract, "protos", root, &manifest)?;
    let includes = contract_paths(contract, "includes", root, &manifest)?;
    let generated = contract
        .get("generated")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| Error::ManifestPreparation {
            path: manifest.clone(),
            message: "contract metadata has no generated directory".to_owned(),
        })?;
    let generated = safe_package_path(root, generated, &manifest)?;
    let dependencies = contract
        .get("dependencies")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| Error::ManifestPreparation {
                    path: manifest.clone(),
                    message: "contract metadata dependencies must be an array".to_owned(),
                })?
                .iter()
                .map(|value| {
                    let table = value.as_table().ok_or_else(|| Error::ManifestPreparation {
                        path: manifest.clone(),
                        message: "contract dependency entries must be inline tables".to_owned(),
                    })?;
                    let field = |name: &str| {
                        table
                            .get(name)
                            .and_then(toml::Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                            .map(str::to_owned)
                            .ok_or_else(|| Error::ManifestPreparation {
                                path: manifest.clone(),
                                message: format!(
                                    "contract dependency entry has no non-empty `{name}`"
                                ),
                            })
                    };
                    let dependency = field("dependency")?;
                    let proto_package = field("proto_package")?;
                    let rust_path = field("rust_path")?;
                    if !proto_package.starts_with('.') || !rust_path.starts_with("::") {
                        return Err(Error::ManifestPreparation {
                            path: manifest.clone(),
                            message: format!(
                                "contract dependency `{dependency}` requires a leading-dot proto_package and absolute rust_path"
                            ),
                        });
                    }
                    Ok(ContractDependency {
                        dependency,
                        proto_package,
                        rust_path,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(LocalContractPackage {
        name,
        root: root.to_owned(),
        protos,
        includes,
        generated,
        dependencies,
    })
}

fn contract_paths(
    contract: &toml::Value,
    field: &str,
    root: &Path,
    manifest: &Path,
) -> Result<Vec<PathBuf>, Error> {
    contract
        .get(field)
        .and_then(toml::Value::as_array)
        .ok_or_else(|| Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!("contract metadata has no {field} array"),
        })?
        .iter()
        .map(|value| {
            let path = value.as_str().ok_or_else(|| Error::ManifestPreparation {
                path: manifest.to_owned(),
                message: format!("contract metadata {field} entries must be strings"),
            })?;
            safe_package_path(root, path, manifest)
        })
        .collect()
}

fn safe_package_path(root: &Path, relative: &str, manifest: &Path) -> Result<PathBuf, Error> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!("contract metadata path `{relative}` must stay inside the package"),
        });
    }
    Ok(root.join(path))
}

pub(crate) fn finalize_robot_api_in_transaction(
    layout: &ProjectLayout,
    document: &RobotDocument,
    sources: &crate::project::SourceSelection,
    options: &CargoOptions,
    transaction: &mut ManifestTransaction,
) -> Result<bool, Error> {
    let RobotDocument::V0 { services, .. } = document;
    if services.is_empty() {
        return Ok(false);
    }
    for selected in sources.services.values() {
        let root = robot_api::package_root(selected)?;
        let package = local_contract_package(&root)?;
        phoxal_build::verify_contract_metadata(
            &package.protos,
            &package.includes,
            &package.generated,
        )
        .map_err(|error| Error::ManifestPreparation {
            path: root.join("Cargo.toml"),
            message: format!("resolved service contract artifacts are stale or invalid: {error}"),
        })?;
        let resolved_dependencies = resolve_contract_dependencies(&package, options)?;
        let mut dependency_bytes = BTreeMap::new();
        for dependency in resolved_dependencies {
            let path = dependency.root.join("generated/phoxal-descriptors.bin");
            let bytes = fs::read(&path).map_err(|source| Error::ManifestPreparation {
                path: path.clone(),
                message: format!("cannot read resolved dependency descriptors: {source}"),
            })?;
            dependency_bytes.entry(dependency.package).or_insert(bytes);
        }
        let dependency_descriptors = dependency_bytes
            .iter()
            .map(|(package, bytes)| phoxal_build::DependencyDescriptor::new(package, bytes))
            .collect::<Vec<_>>();
        phoxal_build::verify_contract_dependencies(&package.generated, &dependency_descriptors)
            .map_err(|error| Error::ManifestPreparation {
                path: root.join("Cargo.toml"),
                message: format!("resolved service dependency closure is incompatible: {error}"),
            })?;
    }
    let candidate = robot_api::finalized_candidate(document, layout.root(), sources)?;
    let generated_root = layout.root().join(robot_api::DIRECTORY);
    let files = [
        (generated_root.join("src/lib.rs"), candidate.lib.as_bytes()),
        (
            generated_root.join("src/contracts.rs"),
            candidate.contracts.as_bytes(),
        ),
        (
            generated_root.join("src/services.rs"),
            candidate.services.as_bytes(),
        ),
    ];
    let changed = files.iter().any(|(path, expected)| {
        fs::read(path)
            .ok()
            .is_none_or(|contents| contents != *expected)
    });
    if !changed {
        return Ok(false);
    }
    if let Some(lock_mode) = match options.lock {
        LockMode::Locked => Some("--locked"),
        LockMode::Frozen => Some("--frozen"),
        LockMode::Unlocked => None,
    } {
        return Err(Error::MissingInitialization {
            path: layout.cargo_manifest().to_owned(),
            dependency: "generated robot_api bindings".to_owned(),
            lock_mode,
        });
    }
    for (path, contents) in files {
        if fs::read(&path).ok().as_deref() == Some(contents) {
            continue;
        }
        transaction.snapshot_managed_file(&path)?;
        atomic_write(&path, contents).map_err(|source| Error::ManifestWrite {
            path: path.clone(),
            source,
        })?;
        let relative = path
            .strip_prefix(layout.root())
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        transaction
            .changes
            .push(PreparationChange::RobotApiFileWritten { path: relative });
    }
    Ok(true)
}

fn robot_api_dependency(package: &str) -> Item {
    let mut requirement = InlineTable::new();
    requirement.insert("package", Value::from(package));
    requirement.insert("path", Value::from(robot_api::DIRECTORY));
    Item::Value(Value::InlineTable(requirement))
}

fn robot_api_dependency_matches(item: &Item, package: &str) -> bool {
    item.as_inline_table().is_some_and(|requirement| {
        requirement.get("package").and_then(Value::as_str) == Some(package)
            && requirement.get("path").and_then(Value::as_str) == Some(robot_api::DIRECTORY)
            && requirement.len() == 2
    })
}

fn acquire_preparation_lock(
    layout: &ProjectLayout,
    manifest: &Path,
) -> Result<(ExclusiveFileLock, PathBuf), Error> {
    let workspace_root = cargo_workspace_root(layout, manifest)?;
    let lock_directory = workspace_root.join("target/phoxal");
    fs::create_dir_all(&lock_directory).map_err(|error| Error::ManifestPreparation {
        path: manifest.to_owned(),
        message: format!("cannot create the preparation lock directory: {error}"),
    })?;
    let lock_path = lock_directory.join("preparation.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!(
                "cannot open project preparation lock {}: {error}",
                lock_path.display()
            ),
        })?;
    match ExclusiveFileLock::try_acquire(lock) {
        Ok(lock) => Ok((lock, workspace_root)),
        Err(TryLockError::WouldBlock) => Err(Error::ManifestPreparation {
            path: manifest.to_owned(),
            message:
                "another cargo phoxal command is preparing this project; retry after it finishes"
                    .to_owned(),
        }),
        Err(TryLockError::Error(error)) => Err(Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!(
                "cannot lock project preparation state {}: {error}",
                lock_path.display()
            ),
        }),
    }
}

fn cargo_workspace_root(layout: &ProjectLayout, manifest: &Path) -> Result<PathBuf, Error> {
    let source = fs::read_to_string(manifest).map_err(|source| Error::ReadManifest {
        path: manifest.to_owned(),
        source,
    })?;
    let document = source
        .parse::<DocumentMut>()
        .map_err(|error| Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!("Cargo.toml is not valid TOML: {error}"),
        })?;

    if let Some(workspace) = document
        .get("package")
        .and_then(Item::as_table)
        .and_then(|package| package.get("workspace"))
        .and_then(Item::as_str)
    {
        let package_root = manifest.parent().unwrap_or_else(|| Path::new("."));
        return canonical_workspace_root(package_root.join(workspace), manifest);
    }

    let mut cursor = layout.root();
    loop {
        let candidate = cursor.join("Cargo.toml");
        let candidate_document = if candidate == manifest {
            document.clone()
        } else {
            match fs::read_to_string(&candidate) {
                Ok(source) => {
                    source
                        .parse::<DocumentMut>()
                        .map_err(|error| Error::ManifestPreparation {
                            path: candidate.clone(),
                            message: format!("Cargo.toml is not valid TOML: {error}"),
                        })?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
                Err(source) => {
                    return Err(Error::ReadManifest {
                        path: candidate,
                        source,
                    });
                }
            }
        };
        if candidate_document
            .get("workspace")
            .is_some_and(Item::is_table)
        {
            return canonical_workspace_root(cursor, manifest);
        }
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent == cursor {
            break;
        }
        cursor = parent;
    }

    canonical_workspace_root(layout.root(), manifest)
}

fn canonical_workspace_root(root: impl AsRef<Path>, manifest: &Path) -> Result<PathBuf, Error> {
    root.as_ref()
        .canonicalize()
        .map_err(|error| Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!("cannot resolve the owning Cargo workspace: {error}"),
        })
}

fn lock_snapshots(root: &Path) -> Result<Vec<LockSnapshot>, Error> {
    let path = root.join("Cargo.lock");
    let contents = match fs::read(&path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(Error::ManifestPreparation {
                path,
                message: format!("cannot snapshot Cargo.lock before preparation: {error}"),
            });
        }
    };
    Ok(vec![LockSnapshot { contents, path }])
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let permissions = match fs::metadata(path) {
        Ok(metadata) => metadata.permissions(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::Permissions::from_mode(0o644)
            }
            #[cfg(not(unix))]
            {
                fs::metadata(parent)?.permissions()
            }
        }
        Err(error) => return Err(error),
    };
    temporary.as_file().set_permissions(permissions)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_preparation_releases_a_lock_even_while_a_descriptor_alias_survives()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("robot.yaml"), "robot: {}\n")?;
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = \"robot\"\nversion = \"0.1.0\"\n",
        )?;
        fs::create_dir_all(directory.path().join("src"))?;
        fs::write(directory.path().join("src/main.rs"), "fn main() {}\n")?;
        let layout = ProjectLayout::discover(directory.path())?;
        let transaction = ensure_required_dependencies(&layout, &CargoOptions::default())?;
        // A concurrent process spawn can briefly inherit the same open file
        // description. Closing our descriptor alone does not release flock.
        let inherited = transaction._lock.clone_descriptor()?;
        transaction.commit();
        let next = ensure_required_dependencies(&layout, &CargoOptions::default())?;
        next.commit();
        drop(inherited);
        Ok(())
    }

    #[test]
    fn concurrent_preparation_fails_before_manifest_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("robot.yaml"), "robot: {}\n")?;
        fs::create_dir_all(directory.path().join("src"))?;
        fs::write(directory.path().join("src/main.rs"), "fn main() {}\n")?;
        let manifest = directory.path().join("Cargo.toml");
        let original = b"[package]\nname = \"robot\"\nversion = \"0.1.0\"\n";
        fs::write(&manifest, original)?;
        let layout = ProjectLayout::discover(directory.path())?;
        let (held, _) = acquire_preparation_lock(&layout, &manifest)?;

        let error = ensure_required_dependencies(&layout, &CargoOptions::default())
            .expect_err("a concurrent preparation lock must be reported");
        assert!(matches!(
            error,
            Error::ManifestPreparation { message, .. }
                if message.contains("another cargo phoxal command")
        ));
        assert_eq!(fs::read(&manifest)?, original);

        drop(held);
        let transaction = ensure_required_dependencies(&layout, &CargoOptions::default())?;
        assert_eq!(transaction.commit().len(), 1);
        Ok(())
    }

    #[test]
    fn preparation_lock_is_shared_by_workspace_members() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"robot-a\", \"robot-b\"]\n",
        )?;
        for name in ["robot-a", "robot-b"] {
            let root = directory.path().join(name);
            fs::create_dir_all(&root)?;
            fs::write(root.join("robot.yaml"), "robot: {}\n")?;
            fs::create_dir_all(root.join("src"))?;
            fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
            fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
            )?;
        }
        let layout_a = ProjectLayout::discover(directory.path().join("robot-a"))?;
        let layout_b = ProjectLayout::discover(directory.path().join("robot-b"))?;
        let (held, workspace_root) =
            acquire_preparation_lock(&layout_a, layout_a.cargo_manifest())?;
        assert_eq!(workspace_root, directory.path().canonicalize()?);

        let error = acquire_preparation_lock(&layout_b, layout_b.cargo_manifest())
            .expect_err("workspace members must share preparation serialization");
        assert!(
            matches!(error, Error::ManifestPreparation { message, .. } if message.contains("another cargo phoxal command"))
        );
        drop(held);
        Ok(())
    }

    #[test]
    fn local_contract_refresh_is_automatic_and_failure_preserves_the_last_candidate()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let service = directory.path().join("motion");
        fs::create_dir_all(service.join("proto/example/motion/v1"))?;
        fs::write(
            service.join("Cargo.toml"),
            r#"[package]
name = "example-motion"
version = "0.1.0"

[package.metadata.phoxal.contract]
protos = ["proto/example/motion/v1/motion.proto"]
includes = ["proto"]
generated = "generated"
"#,
        )?;
        let proto = service.join("proto/example/motion/v1/motion.proto");
        fs::write(
            &proto,
            r#"syntax = "proto3";
package example.motion.v1;
import "google/protobuf/empty.proto";
message Status { bool stopped = 1; }
service Motion { rpc ObserveStatus(google.protobuf.Empty) returns (stream Status); }
"#,
        )?;
        let document: RobotDocument = serde_yaml::from_str(
            r#"schema: phoxal/robot/v0
robot: { id: rover, components: {} }
services:
  motion:
    source: { path: motion }
"#,
        )?;

        let first =
            prepare_local_service_contracts(&document, directory.path(), &CargoOptions::default())?;
        assert!(!first.is_empty());
        let generated = service.join("generated/example.motion.v1.rs");
        let accepted = fs::read(&generated)?;
        assert!(service.join("generated/lib.rs").is_file());
        assert!(service.join("generated/phoxal-descriptors.bin").is_file());
        fs::write(service.join("generated/obsolete.rs"), "stale")?;

        fs::write(
            &proto,
            r#"syntax = "proto3";
package example.motion.v1;
import "google/protobuf/empty.proto";
message Status { bool stopped = 1; uint64 revision = 2; }
service Motion { rpc ObserveStatus(google.protobuf.Empty) returns (stream Status); }
"#,
        )?;
        let second =
            prepare_local_service_contracts(&document, directory.path(), &CargoOptions::default())?;
        assert!(!second.is_empty());
        let refreshed = fs::read(&generated)?;
        assert_ne!(accepted, refreshed);
        assert!(!service.join("generated/obsolete.rs").exists());

        fs::write(&proto, "this is not protobuf")?;
        let error =
            prepare_local_service_contracts(&document, directory.path(), &CargoOptions::default())
                .expect_err("invalid local contract must fail");
        assert!(
            error
                .to_string()
                .contains("local contract generation failed")
        );
        assert_eq!(fs::read(&generated)?, refreshed);
        Ok(())
    }

    #[test]
    fn robot_api_initialization_is_locked_repeatable_and_rolls_back_with_cargo_lock()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::create_dir_all(directory.path().join("src"))?;
        fs::write(directory.path().join("src/main.rs"), "fn main() {}\n")?;
        fs::write(
            directory.path().join("robot.yaml"),
            "schema: phoxal/robot/v0\nrobot: { id: rover, components: {} }\n",
        )?;
        fs::write(
            directory.path().join("Cargo.toml"),
            r#"[package]
name = "robot"
version = "0.1.0"
edition = "2024"

[dependencies]
phoxal-supervisor = "1"
"#,
        )?;
        let original_lock = b"# accepted lock\n";
        fs::write(directory.path().join("Cargo.lock"), original_lock)?;

        let service = directory.path().join("motion");
        fs::create_dir_all(service.join("proto/example/motion/v1"))?;
        fs::write(
            service.join("Cargo.toml"),
            r#"[package]
name = "example-motion"
version = "0.1.0"

[package.metadata.phoxal.contract]
protos = ["proto/example/motion/v1/motion.proto"]
includes = ["proto"]
generated = "generated"
"#,
        )?;
        fs::write(
            service.join("proto/example/motion/v1/motion.proto"),
            r#"syntax = "proto3";
package example.motion.v1;
import "google/protobuf/empty.proto";
message Status { bool stopped = 1; }
service Motion { rpc ObserveStatus(google.protobuf.Empty) returns (stream Status); }
"#,
        )?;
        let document: RobotDocument = serde_yaml::from_str(
            r#"schema: phoxal/robot/v0
robot: { id: rover, components: {} }
services:
  motion:
    source: { path: motion }
"#,
        )?;
        let layout = ProjectLayout::discover(directory.path())?;
        let mut first = ensure_required_dependencies(&layout, &CargoOptions::default())?;
        prepare_robot_api_in_transaction(&layout, &document, &CargoOptions::default(), &mut first)?;
        first.commit();

        let accepted_manifest = fs::read(layout.cargo_manifest())?;
        let accepted_api = fs::read(directory.path().join(".phoxal/robot-api/Cargo.toml"))?;
        let locked = CargoOptions {
            lock: LockMode::Locked,
            ..CargoOptions::default()
        };
        let mut repeat = ensure_required_dependencies(&layout, &locked)?;
        prepare_robot_api_in_transaction(&layout, &document, &locked, &mut repeat)?;
        assert!(repeat.commit().is_empty());

        let replacement: RobotDocument = serde_yaml::from_str(
            r#"schema: phoxal/robot/v0
robot: { id: replacement, components: {} }
services:
  motion:
    source: { path: motion }
"#,
        )?;
        let mut failed = ensure_required_dependencies(&layout, &CargoOptions::default())?;
        prepare_robot_api_in_transaction(
            &layout,
            &replacement,
            &CargoOptions::default(),
            &mut failed,
        )?;
        fs::write(directory.path().join("Cargo.lock"), b"# rejected lock\n")?;
        drop(failed);

        assert_eq!(fs::read(layout.cargo_manifest())?, accepted_manifest);
        assert_eq!(
            fs::read(directory.path().join("Cargo.lock"))?,
            original_lock
        );
        assert_eq!(
            fs::read(directory.path().join(".phoxal/robot-api/Cargo.toml"))?,
            accepted_api
        );
        Ok(())
    }

    #[test]
    fn local_contract_preparation_resolves_transitive_owner_descriptors_through_cargo()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let base = directory.path().join("base");
        let derived = directory.path().join("derived");
        for root in [&base, &derived] {
            fs::create_dir_all(root.join("generated"))?;
            fs::write(root.join("generated/lib.rs"), "")?;
        }
        fs::create_dir_all(base.join("proto/example/base/v1"))?;
        fs::write(
            base.join("Cargo.toml"),
            r#"[package]
name = "example-base"
version = "0.1.0"
edition = "2024"

[lib]
path = "generated/lib.rs"

[package.metadata.phoxal.contract]
protos = ["proto/example/base/v1/base.proto"]
includes = ["proto"]
generated = "generated"
"#,
        )?;
        fs::write(
            base.join("proto/example/base/v1/base.proto"),
            "syntax = \"proto3\"; package example.base.v1; message Shared { uint64 sequence = 1; }\n",
        )?;

        fs::create_dir_all(derived.join("proto/example/derived/v1"))?;
        fs::write(
            derived.join("Cargo.toml"),
            r#"[package]
name = "example-derived"
version = "0.1.0"
edition = "2024"

[lib]
path = "generated/lib.rs"

[dependencies]
base = { package = "example-base", path = "../base" }

[package.metadata.phoxal.contract]
protos = ["proto/example/derived/v1/derived.proto"]
includes = ["proto"]
generated = "generated"
dependencies = [
  { dependency = "base", proto_package = ".example.base.v1", rust_path = "::base" },
]
"#,
        )?;
        fs::write(
            derived.join("proto/example/derived/v1/derived.proto"),
            r#"syntax = "proto3";
package example.derived.v1;
import "google/protobuf/empty.proto";
import "phoxal/api.proto";
import "example/base/v1/base.proto";
service Derived {
  rpc Current(google.protobuf.Empty) returns (stream example.base.v1.Shared) {
    option (phoxal.api.retained_latest) = true;
  }
}
"#,
        )?;
        let document: RobotDocument = serde_yaml::from_str(
            r#"schema: phoxal/robot/v0
robot: { id: rover, components: {} }
services:
  derived:
    source: { path: derived }
"#,
        )?;

        let changes =
            prepare_local_service_contracts(&document, directory.path(), &CargoOptions::default())?;
        assert!(changes.iter().any(|change| {
            matches!(change, PreparationChange::ServiceContractFileWritten { package, .. } if package == "example-base")
        }));
        assert!(changes.iter().any(|change| {
            matches!(change, PreparationChange::ServiceContractFileWritten { package, .. } if package == "example-derived")
        }));
        let generated = fs::read_to_string(derived.join("generated/example.derived.v1.rs"))?;
        assert!(generated.contains("::base::Shared"));
        let metadata = fs::read_to_string(derived.join("generated/phoxal-contract.json"))?;
        assert!(metadata.contains("example-base"));
        Ok(())
    }
}
