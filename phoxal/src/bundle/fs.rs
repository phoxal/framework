//! Bundle root resolution, staged writes, and plain reads.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::manifest::ManifestDocument;

use crate::bundle::{BundleError, BundlePath, MANIFEST_FILE};

/// The mode the writer gives every staged directory and executable.
const EXECUTABLE_MODE: u32 = 0o755;
/// The mode the writer gives every staged data file.
const DATA_MODE: u32 = 0o644;

/// A bundle root that has been proven to be a directory.
#[derive(Clone, Debug)]
pub(crate) struct BundleRoot {
    path: PathBuf,
}

impl BundleRoot {
    pub(crate) fn open(requested: &Path) -> Result<Self, BundleError> {
        let metadata = std::fs::metadata(requested).map_err(|source| BundleError::Root {
            path: requested.to_path_buf(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(BundleError::NotDirectory(requested.to_path_buf()));
        }
        Ok(Self {
            path: requested.to_path_buf(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn relocate(&mut self, path: PathBuf) {
        self.path = path;
    }
}

pub(crate) fn open_bundle_file(
    root: &BundleRoot,
    path: &BundlePath,
) -> Result<std::fs::File, BundleError> {
    let filesystem_path = path.filesystem_path(root.path());
    match std::fs::File::open(&filesystem_path) {
        Ok(file) => Ok(file),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(BundleError::MissingFile {
                path: filesystem_path,
            })
        }
        Err(source) => Err(BundleError::ReadFile {
            path: filesystem_path,
            source,
        }),
    }
}

/// Open one staged executable, refusing anything that is not a runnable file.
///
/// This is a check on the writer's *input*, not on the bundle: a source that is
/// a directory, a symlink or a non-executable file produces a bundle whose
/// launcher fails much later with a far worse diagnostic.
pub(crate) fn open_executable_source(path: &Path) -> Result<std::fs::File, BundleError> {
    let file = std::fs::File::open(path).map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BundleError::NotExecutable {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(file)
}

pub(crate) fn ensure_staging_directory(
    root: &BundleRoot,
    relative: &str,
) -> Result<(), BundleError> {
    let mut path = root.path().to_path_buf();
    for component in relative.split('/') {
        path.push(component);
        match std::fs::create_dir(&path) {
            Ok(()) => set_mode(&path, EXECUTABLE_MODE)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(BundleError::ReadFile { path, source }),
        }
    }
    Ok(())
}

fn create_staging_file(
    root: &BundleRoot,
    path: &BundlePath,
    mode: u32,
) -> Result<std::fs::File, BundleError> {
    if let Some((parent, _)) = path.as_str().rsplit_once('/') {
        ensure_staging_directory(root, parent)?;
    }
    let filesystem_path = path.filesystem_path(root.path());
    let file =
        std::fs::File::create_new(&filesystem_path).map_err(|source| BundleError::ReadFile {
            path: filesystem_path.clone(),
            source,
        })?;
    set_mode(&filesystem_path, mode)?;
    Ok(file)
}

/// Move a completed staging root onto its final name. The rename is the one
/// step that makes a bundle visible, so an interrupted build leaves the target
/// name either absent or holding a complete bundle.
pub(crate) fn publish_staging_root(staged: &Path, target: &Path) -> Result<(), BundleError> {
    std::fs::rename(staged, target).map_err(|source| BundleError::ReadFile {
        path: target.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), BundleError> {
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).map_err(
        |source| BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        },
    )
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), BundleError> {
    Ok(())
}

pub(crate) fn read_manifest_document(root: &BundleRoot) -> Result<ManifestDocument, BundleError> {
    let manifest_path = root.path().join(MANIFEST_FILE);
    // An absent or unreadable manifest is reported as a manifest failure rather
    // than as one more missing file: it is the one file a bundle is.
    let mut file =
        open_bundle_file(root, &BundlePath::new(MANIFEST_FILE)?).map_err(|error| match error {
            BundleError::ReadFile { path, source } => BundleError::ReadManifest { path, source },
            BundleError::MissingFile { path } => BundleError::ReadManifest {
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, MANIFEST_FILE),
            },
            other => other,
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| BundleError::ReadManifest {
            path: manifest_path,
            source,
        })?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn write_new_file(
    root: &BundleRoot,
    path: &BundlePath,
    bytes: &[u8],
) -> Result<(), BundleError> {
    let mut file = create_staging_file(root, path, DATA_MODE)?;
    let diagnostic = path.filesystem_path(root.path());
    std::io::Write::write_all(&mut file, bytes).map_err(|source| BundleError::ReadFile {
        path: diagnostic,
        source,
    })
}

/// Copy one staged executable into `bin/`, marking it executable.
pub(crate) fn copy_executable_source(
    root: &BundleRoot,
    source: &Path,
    destination: &BundlePath,
) -> Result<(), BundleError> {
    let mut input = open_executable_source(source)?;
    let mut output = create_staging_file(root, destination, EXECUTABLE_MODE)?;
    let diagnostic = destination.filesystem_path(root.path());
    std::io::copy(&mut input, &mut output).map_err(|error| BundleError::ReadFile {
        path: diagnostic,
        source: error,
    })?;
    Ok(())
}

pub(crate) fn prepare_publish_parent(root: &Path) -> Result<PathBuf, BundleError> {
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    // A host may expose its temporary directory through a compatibility
    // symlink (for example macOS `/var`), so the parent is resolved once and
    // the bundle is published under that resolved name.
    let canonical_parent = parent
        .canonicalize()
        .map_err(|source| BundleError::ReadFile {
            path: parent.to_path_buf(),
            source,
        })?;
    let metadata =
        std::fs::symlink_metadata(&canonical_parent).map_err(|source| BundleError::ReadFile {
            path: canonical_parent.clone(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(BundleError::NotDirectory(canonical_parent));
    }
    let name = root.file_name().ok_or_else(|| BundleError::ReadFile {
        path: root.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid bundle name"),
    })?;
    Ok(canonical_parent.join(name))
}

/// A bundle is published onto a free name, so an installed bundle is never
/// modified in place.
pub(crate) fn reject_existing_target(root: &Path) -> Result<(), BundleError> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => Err(BundleError::TargetExists(root.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BundleError::ReadFile {
            path: root.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn create_staging_root(target: &Path) -> Result<PathBuf, BundleError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BundleError::ReadFile {
            path: target.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid bundle name"),
        })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for attempt in 0..100u32 {
        let staged = parent.join(format!(
            ".{name}.staging-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&staged) {
            Ok(()) => {
                set_mode(&staged, EXECUTABLE_MODE)?;
                return Ok(staged);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(BundleError::ReadFile {
                    path: staged,
                    source,
                });
            }
        }
    }
    Err(BundleError::ReadFile {
        path: parent.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::AlreadyExists, "staging name exhausted"),
    })
}
