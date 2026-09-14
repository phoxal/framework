//! Atomic preparation of the authored Cargo graph.
//!
//! The project compiler owns only the small set of infrastructure defaults
//! that are part of every hardware execution.  They are ordinary dependency
//! entries in the robot's Cargo manifest, so Cargo remains the one resolver and
//! the resulting lockfile remains inspectable by users and editors.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::TryLockError;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use crate::ProjectLayout;
use crate::cargo::{CargoOptions, LockMode};
use crate::error::Error;
use crate::file_lock::ExclusiveFileLock;

/// The official package key used by the mandatory supervisor dependency.
pub const SUPERVISOR_DEPENDENCY_KEY: &str = "phoxal-supervisor";
/// The unconstrained package version requirement used for a fresh project.
/// Existing requirements and locked selections always take precedence.
pub const SUPERVISOR_VERSION_REQUIREMENT: &str = "*";
/// The configured registry containing official Phoxal packages.
pub const SUPERVISOR_REGISTRY: &str = "phoxal";

/// One visible change made by ordinary unlocked preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparationChange {
    /// Exact root manifest dependency key added by the tool.
    pub dependency: String,
    /// Human-readable Cargo requirement written for the dependency.
    pub requirement: String,
}

/// One visible change made by scenario preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioPreparationChange {
    /// `[[test]]` target added under the given name with the given path.
    TestTargetAdded { name: String, path: String },
    /// `[[test]]` target removed after the last scenario was deleted.
    TestTargetRemoved { name: String },
    /// `phoxal` feature gate added to the dev-dependencies entry.
    FeatureAdded { feature: String },
    /// `phoxal` feature gate removed from the dev-dependencies entry.
    FeatureRemoved { feature: String },
    /// Harness source regenerated under the given path.
    HarnessWritten { path: String },
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
    changes: Vec<PreparationChange>,
}

impl ManifestTransaction {
    /// Finish a successful preparation and release its source mutation lock.
    pub(crate) fn commit(self) -> Vec<PreparationChange> {
        self.changes
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
        Ok(())
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
            changes: Vec::new(),
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
        changes: vec![PreparationChange {
            dependency: SUPERVISOR_DEPENDENCY_KEY.to_owned(),
            requirement: format!(
                "version = \"{SUPERVISOR_VERSION_REQUIREMENT}\", registry = \"{SUPERVISOR_REGISTRY}\""
            ),
        }],
    })
}

// --- Scenario preparation entry point ---------------------------------

pub(crate) const SCENARIO_TEST_TARGET_NAME: &str = "phoxal-scenarios";
pub(crate) const SCENARIO_HARNESS_RELATIVE_PATH: &str = ".phoxal/generated/scenarios/main.rs";

/// Production scenario preparation entry point. Acquires the same
/// workspace file lock as ordinary preparation, idempotently edits the
/// manifest, writes the generated harness, and commits the manifest
/// atomically. Locked/frozen modes refuse *before* mutating when the
/// persistent setup is missing.
pub(crate) fn prepare_scenario_target(
    layout: &ProjectLayout,
    options: &CargoOptions,
) -> Result<Vec<ScenarioPreparationChange>, Error> {
    options.validate()?;
    let (lock, _workspace_root) = acquire_preparation_lock(layout, layout.cargo_manifest())?;
    let result = prepare_scenario_target_locked(layout, options, layout.root(), &lock);
    drop(lock);
    result
}

fn prepare_scenario_target_locked(
    layout: &ProjectLayout,
    options: &CargoOptions,
    robot_root: &Path,
    _lock: &ExclusiveFileLock,
) -> Result<Vec<ScenarioPreparationChange>, Error> {
    options.validate()?;
    let manifest = layout.cargo_manifest();
    let manifest_text =
        fs::read_to_string(manifest).map_err(|source| Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!("cannot read authored manifest: {source}"),
        })?;
    let mut document: DocumentMut =
        manifest_text
            .parse()
            .map_err(|source| Error::ManifestPreparation {
                path: manifest.to_owned(),
                message: format!("cannot parse authored manifest: {source}"),
            })?;
    let mut changes = Vec::new();
    prepare_scenario_target_in_document(layout, robot_root, &mut document, &mut changes)?;
    let prepared = document.to_string().into_bytes();
    if prepared != manifest_text.as_bytes() && !changes.is_empty() {
        match options.lock {
            LockMode::Unlocked => {}
            LockMode::Locked | LockMode::Frozen => {
                return Err(Error::ManifestPreparation {
                    path: manifest.to_owned(),
                    message: format!(
                        "scenario setup needs to add `[[test]] {SCENARIO_TEST_TARGET_NAME}` and \
                         `[dev-dependencies] phoxal.features = [\"scenario\"]`; refusing to \
                         mutate the manifest while {:?} is in effect. Re-run without \
                         --locked / --frozen.",
                        options.lock
                    ),
                });
            }
        }
        atomic_write(manifest, &prepared).map_err(|source| Error::ManifestPreparation {
            path: manifest.to_owned(),
            message: format!("cannot persist manifest: {source}"),
        })?;
    }
    Ok(changes)
}

fn prepare_scenario_target_in_document(
    layout: &ProjectLayout,
    robot_root: &Path,
    document: &mut DocumentMut,
    changes: &mut Vec<ScenarioPreparationChange>,
) -> Result<(), Error> {
    let discovered = match super::scenario::discover_scenarios(robot_root) {
        Ok(list) => list,
        Err(error) => {
            return Err(Error::ManifestPreparation {
                path: layout.cargo_manifest().to_owned(),
                message: format!("scenario discovery failed: {error}"),
            });
        }
    };
    if discovered.is_empty() {
        if let Some(true) = remove_scenario_test_target(document) {
            changes.push(ScenarioPreparationChange::TestTargetRemoved {
                name: SCENARIO_TEST_TARGET_NAME.to_owned(),
            });
        }
        if remove_scenario_dev_dependency_feature(document) {
            changes.push(ScenarioPreparationChange::FeatureRemoved {
                feature: "scenario".to_owned(),
            });
        }
        let harness_changed =
            write_scenario_harness(robot_root, &discovered).map_err(|message| {
                Error::ManifestPreparation {
                    path: layout.cargo_manifest().to_owned(),
                    message,
                }
            })?;
        if harness_changed {
            changes.push(ScenarioPreparationChange::HarnessWritten {
                path: SCENARIO_HARNESS_RELATIVE_PATH.to_owned(),
            });
        }
        return Ok(());
    }
    let test_target =
        ensure_scenario_test_target(document).map_err(|message| Error::ManifestPreparation {
            path: layout.cargo_manifest().to_owned(),
            message,
        })?;
    if let Some(change) = test_target {
        changes.push(change);
    }
    let dev_dep =
        ensure_scenario_dev_dependency(document).map_err(|message| Error::ManifestPreparation {
            path: layout.cargo_manifest().to_owned(),
            message,
        })?;
    if let Some(change) = dev_dep {
        changes.push(change);
    }
    let harness_changed = write_scenario_harness(robot_root, &discovered).map_err(|message| {
        Error::ManifestPreparation {
            path: layout.cargo_manifest().to_owned(),
            message,
        }
    })?;
    if harness_changed {
        changes.push(ScenarioPreparationChange::HarnessWritten {
            path: SCENARIO_HARNESS_RELATIVE_PATH.to_owned(),
        });
    }
    Ok(())
}

fn write_scenario_harness(
    robot_root: &Path,
    discovered: &[crate::scenario::DiscoveredScenario],
) -> Result<bool, String> {
    let harness_path = robot_root.join(SCENARIO_HARNESS_RELATIVE_PATH);
    if let Some(parent) = harness_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|source| format!("cannot create `{}`: {source}", parent.display()))?;
    }
    let harness_source = super::scenario::generate_harness_source(robot_root, discovered);
    let next = harness_source.into_bytes();
    let previous = fs::read(&harness_path).ok();
    if previous.as_deref() != Some(next.as_slice()) {
        atomic_write(&harness_path, &next)
            .map_err(|source| format!("cannot write generated harness: {source}"))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn ensure_scenario_test_target(
    document: &mut DocumentMut,
) -> Result<Option<ScenarioPreparationChange>, String> {
    if let Some(existing) = lookup_scenario_test_target(document) {
        let path = existing.get("path").and_then(Item::as_str);
        let harness = existing.get("harness").and_then(Item::as_bool);
        let test_flag = existing.get("test").and_then(Item::as_bool);
        if path == Some(SCENARIO_HARNESS_RELATIVE_PATH)
            && harness == Some(false)
            && test_flag == Some(false)
        {
            return Ok(None);
        }
        return Err(format!(
            "authored `[[test]] name = \"{SCENARIO_TEST_TARGET_NAME}\"` exists with a different \
             path or harness setting ({path:?}, harness={harness:?}, test={test_flag:?}); \
             refusing to overwrite",
        ));
    }
    match document.get_mut("test") {
        Some(item) => match item.as_array_of_tables_mut() {
            Some(arr) => {
                arr.push(build_scenario_test_table());
                Ok(Some(ScenarioPreparationChange::TestTargetAdded {
                    name: SCENARIO_TEST_TARGET_NAME.to_owned(),
                    path: SCENARIO_HARNESS_RELATIVE_PATH.to_owned(),
                }))
            }
            None => Err(
                "`[test]]` exists but is not an array-of-tables; cannot add the scenario target"
                    .to_owned(),
            ),
        },
        None => {
            let mut arr = ArrayOfTables::new();
            arr.push(build_scenario_test_table());
            document["test"] = Item::ArrayOfTables(arr);
            Ok(Some(ScenarioPreparationChange::TestTargetAdded {
                name: SCENARIO_TEST_TARGET_NAME.to_owned(),
                path: SCENARIO_HARNESS_RELATIVE_PATH.to_owned(),
            }))
        }
    }
}

fn lookup_scenario_test_target(document: &DocumentMut) -> Option<Table> {
    let item = document.get("test")?;
    let tests = item.as_array_of_tables()?;
    tests
        .iter()
        .find(|t| t.get("name").and_then(Item::as_str) == Some(SCENARIO_TEST_TARGET_NAME))
        .cloned()
}

fn build_scenario_test_table() -> Table {
    let mut table = Table::new();
    table["name"] = Item::Value(Value::from(SCENARIO_TEST_TARGET_NAME));
    table["path"] = Item::Value(Value::from(SCENARIO_HARNESS_RELATIVE_PATH));
    table["harness"] = Item::Value(Value::from(false));
    table["test"] = Item::Value(Value::from(false));
    table
}

fn ensure_scenario_dev_dependency(
    document: &mut DocumentMut,
) -> Result<Option<ScenarioPreparationChange>, String> {
    let has_phoxal_dev_dep = document
        .get("dev-dependencies")
        .and_then(Item::as_table)
        .map(|t| t.contains_key("phoxal"))
        .unwrap_or(false);
    if !has_phoxal_dev_dep {
        let mirror = build_phoxal_dev_dependency_from_existing(document)?;
        let dev_table = ensure_dev_dependencies_table(document)?;
        dev_table.insert("phoxal", mirror);
        return Ok(Some(ScenarioPreparationChange::FeatureAdded {
            feature: "scenario".to_owned(),
        }));
    }
    let entry = document
        .get_mut("dev-dependencies")
        .and_then(Item::as_table_mut)
        .and_then(|t| t.get_mut("phoxal"))
        .ok_or_else(|| "missing phoxal dev-dependency after existence check".to_owned())?;
    let value = entry.as_value_mut().ok_or_else(|| {
        "`phoxal` dev-dependency is not a value; cannot add scenario feature".to_owned()
    })?;
    let inline = match value.as_inline_table_mut() {
        Some(t) => t,
        None => {
            return Err(
                "`phoxal` dev-dependency is not an inline table; cannot inspect features"
                    .to_owned(),
            );
        }
    };
    let already_has = inline
        .get("features")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("scenario")))
        .unwrap_or(false);
    if already_has {
        return Ok(None);
    }
    match inline.get_mut("features") {
        Some(features) => {
            let arr = features
                .as_array_mut()
                .ok_or_else(|| "phoxal `features` is not an array".to_owned())?;
            arr.push("scenario");
        }
        None => {
            inline.insert(
                "features",
                Value::Array(Array::from_iter(["scenario".to_owned()])),
            );
        }
    }
    Ok(Some(ScenarioPreparationChange::FeatureAdded {
        feature: "scenario".to_owned(),
    }))
}

fn ensure_dev_dependencies_table(document: &mut DocumentMut) -> Result<&mut Table, String> {
    if document.get("dev-dependencies").is_none() {
        document["dev-dependencies"] = Item::Table(Table::new());
    }
    document
        .get_mut("dev-dependencies")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| "`[dev-dependencies]` exists but is not a table".to_owned())
}

fn build_phoxal_dev_dependency_from_existing(document: &DocumentMut) -> Result<Item, String> {
    let sources = ["dependencies", "workspace.dependencies"];
    for path in sources {
        if let Some(value) = lookup_phoxal_dependency(document, path) {
            let mut inline = match value {
                Value::InlineTable(table) => table.clone(),
                _ => {
                    return Err(format!(
                        "authored `phoxal` dependency under `[{path}]` is not an inline table; \
                         cannot mirror its coordinates into [dev-dependencies]"
                    ));
                }
            };
            // When `[dependencies] phoxal` only inherits from the workspace
            // (`workspace = true` and nothing else of substance), follow
            // the inheritance: the workspace-declared coordinates live in
            // `[workspace.dependencies] phoxal`. Falling through here keeps
            // the dev-dependency aligned with the real source.
            if path == "dependencies" && is_workspace_inherit_only(&inline) {
                continue;
            }
            let existing_features = inline
                .get("features")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut merged = existing_features;
            if !merged.iter().any(|f| f == "scenario") {
                merged.push("scenario".to_owned());
            }
            inline.insert("features", Value::Array(Array::from_iter(merged)));
            return Ok(Item::Value(Value::InlineTable(inline)));
        }
    }
    Err(
        "no authored `[dependencies] phoxal` or `[workspace.dependencies] phoxal` entry to \
         mirror; cannot author coordinates on your behalf. Add `phoxal` to one of those tables \
         first."
            .to_owned(),
    )
}

/// True when the inline table contains `workspace = true` and no
/// substantive coordinate — i.e. it is just inheriting from
/// `[workspace.dependencies]` rather than declaring its own.
fn is_workspace_inherit_only(inline: &InlineTable) -> bool {
    let inherits = inline
        .get("workspace")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !inherits {
        return false;
    }
    for key in [
        "path",
        "git",
        "version",
        "registry",
        "features",
        "default-features",
    ] {
        if inline.contains_key(key) {
            return false;
        }
    }
    true
}

fn lookup_phoxal_dependency<'a>(document: &'a DocumentMut, section: &str) -> Option<&'a Value> {
    // `toml_edit::Table::get` is a single-key lookup, not a dotted-path
    // traversal, so a section path like `workspace.dependencies` must be
    // descended by hand. Every segment must materialise as a regular
    // `Table` because `[workspace.dependencies]` is a section header, not
    // an inline table. Once we have reached the section, we look up the
    // hardcoded `phoxal` key inside it and return its inline-table value.
    let mut segments = section.split('.');
    let last = segments.next_back()?;
    let mut current: &Item = document.as_item();
    for segment in segments {
        let next = current.get(segment)?;
        if !next.is_table() {
            return None;
        }
        current = next;
    }
    let section_item = current.get(last)?;
    if !section_item.is_table() {
        return None;
    }
    section_item.get("phoxal")?.as_value()
}

fn remove_scenario_test_target(document: &mut DocumentMut) -> Option<bool> {
    let Some(item) = document.get_mut("test") else {
        return Some(false);
    };
    let Some(tests) = item.as_array_of_tables_mut() else {
        return Some(false);
    };
    let mut removed = false;
    let mut idx = 0;
    while idx < tests.len() {
        let name_matches = tests
            .get(idx)
            .and_then(|t| t.get("name"))
            .and_then(Item::as_str)
            == Some(SCENARIO_TEST_TARGET_NAME);
        if name_matches {
            tests.remove(idx);
            removed = true;
        } else {
            idx += 1;
        }
    }
    if tests.is_empty() {
        document.as_table_mut().remove("test");
    }
    Some(removed)
}

fn remove_scenario_dev_dependency_feature(document: &mut DocumentMut) -> bool {
    let Some(dev) = document.get_mut("dev-dependencies") else {
        return false;
    };
    let Some(table) = dev.as_table_mut() else {
        return false;
    };
    let Some(entry) = table.get_mut("phoxal") else {
        return false;
    };
    let Some(value) = entry.as_value_mut() else {
        return false;
    };
    let Some(inline) = value.as_inline_table_mut() else {
        return false;
    };
    let mut removed = false;
    if let Some(features) = inline.get_mut("features")
        && let Some(arr) = features.as_array_mut()
    {
        let mut idx = 0;
        while idx < arr.len() {
            let is_scenario = arr.get(idx).and_then(|v| v.as_str()) == Some("scenario");
            if is_scenario {
                arr.remove(idx);
                removed = true;
            } else {
                idx += 1;
            }
        }
        if arr.is_empty() {
            inline.remove("features");
        }
    }
    removed
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
}
