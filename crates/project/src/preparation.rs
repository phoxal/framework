//! Atomic preparation of the authored Cargo graph.
//!
//! The project compiler owns only the small set of infrastructure defaults
//! that are part of every hardware execution.  They are ordinary dependency
//! entries in the robot's Cargo manifest, so Cargo remains the one resolver and
//! the resulting lockfile remains inspectable by users and editors.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::ProjectLayout;
use crate::cargo::{CargoOptions, LockMode};
use crate::error::Error;

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
#[derive(Debug, Clone)]
pub(crate) struct ManifestTransaction {
    manifest: PathBuf,
    original_manifest: Vec<u8>,
    locks: Vec<LockSnapshot>,
    changes: Vec<PreparationChange>,
}

impl ManifestTransaction {
    /// Returns the changes that ordinary preparation applied.
    #[must_use]
    pub(crate) fn changes(&self) -> &[PreparationChange] {
        &self.changes
    }

    /// Restores all files captured before an unsuccessful preparation.
    pub(crate) fn rollback(&self) -> Result<(), Error> {
        if self.changes.is_empty() {
            return Ok(());
        }
        atomic_write(&self.manifest, &self.original_manifest).map_err(|source| {
            Error::ManifestRestore {
                path: self.manifest.clone(),
                source,
            }
        })?;
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

    let has_supervisor = document
        .get("dependencies")
        .and_then(Item::as_table)
        .is_some_and(|dependencies| dependencies.contains_key(SUPERVISOR_DEPENDENCY_KEY));
    if has_supervisor {
        return Ok(ManifestTransaction {
            manifest,
            original_manifest,
            locks: Vec::new(),
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

    // Capture every candidate workspace lock before the manifest changes.
    // Cargo metadata may update any one of these paths after the write, and a
    // failed preparation must restore the exact pre-command bytes.
    let locks = lock_snapshots(layout.root())?;
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

fn lock_snapshots(root: &Path) -> Result<Vec<LockSnapshot>, Error> {
    let mut snapshots = Vec::new();
    let mut cursor = root;
    loop {
        let path = cursor.join("Cargo.lock");
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
        snapshots.push(LockSnapshot { contents, path });
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent == cursor {
            break;
        }
        cursor = parent;
    }
    Ok(snapshots)
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
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
