//! Atomic preparation of the authored Cargo graph.
//!
//! The project compiler owns only the small set of infrastructure defaults
//! that are part of every hardware execution.  They are ordinary dependency
//! entries in the robot's Cargo manifest, so Cargo remains the one resolver and
//! the resulting lockfile remains inspectable by users and editors.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::{FileExt, TryLockError};
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
#[derive(Debug)]
pub(crate) struct ManifestTransaction {
    _lock: File,
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

fn acquire_preparation_lock(
    layout: &ProjectLayout,
    manifest: &Path,
) -> Result<(File, PathBuf), Error> {
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
    match FileExt::try_lock(&lock) {
        Ok(()) => Ok((lock, workspace_root)),
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

        FileExt::unlock(&held)?;
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
        FileExt::unlock(&held)?;
        drop(held);
        Ok(())
    }
}
