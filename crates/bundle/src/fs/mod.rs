//! Pinned-root and filesystem entry points for bundle reads.

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::{
    create_staging_file, ensure_staging_directory, mark_staging_root_ready, open_bundle_file,
    open_executable_source, publish_staging_root, require_layout_directories,
};

#[cfg(not(unix))]
pub(crate) fn open_bundle_file(
    root: &BundleRoot,
    path: &BundlePath,
) -> Result<std::fs::File, BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: path.filesystem_path(root.path()),
    })
}

#[cfg(not(unix))]
pub(crate) fn open_executable_source(path: &Path) -> Result<std::fs::File, BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: path.to_path_buf(),
    })
}

#[cfg(not(unix))]
pub(crate) fn create_staging_file(
    root: &BundleRoot,
    path: &BundlePath,
    _mode: u32,
) -> Result<std::fs::File, BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: path.filesystem_path(root.path()),
    })
}

#[cfg(not(unix))]
pub(crate) fn ensure_staging_directory(
    root: &BundleRoot,
    relative: &str,
) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().join(relative),
    })
}

#[cfg(not(unix))]
pub(crate) fn mark_staging_root_ready(root: &BundleRoot) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}

#[cfg(not(unix))]
pub(crate) fn publish_staging_root(staged: &Path, target: &Path) -> Result<(), BundleError> {
    let _ = staged;
    Err(BundleError::UnsupportedAtomicPublish {
        path: target.to_path_buf(),
    })
}

#[cfg(not(unix))]
pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::*;

/// A bundle root pinned to one directory object for the lifetime of a load.
#[derive(Clone)]
pub(crate) struct BundleRoot {
    path: PathBuf,
    #[cfg(unix)]
    pub(crate) fd: Arc<std::os::fd::OwnedFd>,
}

impl fmt::Debug for BundleRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BundleRoot")
            .field("path", &self.path)
            .finish()
    }
}

impl BundleRoot {
    pub(crate) fn open(requested: &Path) -> Result<Self, BundleError> {
        #[cfg(unix)]
        {
            Ok(Self {
                path: requested.to_path_buf(),
                fd: unix::open_root(requested)?,
            })
        }
        #[cfg(not(unix))]
        {
            Err(BundleError::UnsupportedSecureOpen {
                path: requested.to_path_buf(),
            })
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn relocate(&mut self, path: PathBuf) {
        self.path = path;
    }
}

pub(crate) fn read_runtime_document(root: &BundleRoot) -> Result<RuntimeDocument, BundleError> {
    let runtime_path = root.path().join(RUNTIME_FILE);
    let mut runtime_file =
        open_bundle_file(root, &BundlePath::new(RUNTIME_FILE)?).map_err(|error| match error {
            BundleError::ReadFile { path, source } => BundleError::ReadDocument { path, source },
            BundleError::MissingFile { path } => BundleError::ReadDocument {
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, RUNTIME_FILE),
            },
            other => other,
        })?;
    #[cfg(unix)]
    unix::verify_data_file(&runtime_file, &runtime_path)?;
    let mut bytes = Vec::new();
    runtime_file
        .read_to_end(&mut bytes)
        .map_err(|source| BundleError::ReadDocument {
            path: runtime_path,
            source,
        })?;
    crate::document::decode(&bytes)
}

pub(crate) fn write_new_file(
    root: &BundleRoot,
    path: &BundlePath,
    bytes: &[u8],
) -> Result<(), BundleError> {
    let mut file = create_staging_file(root, path, 0o644)?;
    let diagnostic = path.filesystem_path(root.path());
    std::io::Write::write_all(&mut file, bytes).map_err(|source| BundleError::ReadFile {
        path: diagnostic,
        source,
    })
}

pub(crate) fn copy_executable_source(
    root: &BundleRoot,
    source: &BinarySource,
    destination: &BundlePath,
    expected_digest: Sha256Digest,
    expected_size: u64,
) -> Result<(), BundleError> {
    let mut input = source.reader()?;
    let mut output = create_staging_file(root, destination, 0o755)?;
    let diagnostic = destination.filesystem_path(root.path());
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|source_error| BundleError::ReadFile {
                path: source.path().to_path_buf(),
                source: source_error,
            })?;
        if count == 0 {
            break;
        }
        std::io::Write::write_all(&mut output, &buffer[..count]).map_err(|source_error| {
            BundleError::ReadFile {
                path: diagnostic.clone(),
                source: source_error,
            }
        })?;
        hasher.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| BundleError::Size {
                path: diagnostic.clone(),
                expected: expected_size,
                actual: u64::MAX,
            })?;
    }
    let actual = Sha256Digest(hasher.finalize().into());
    if total != expected_size {
        return Err(BundleError::Size {
            path: diagnostic.clone(),
            expected: expected_size,
            actual: total,
        });
    }
    if actual != expected_digest {
        return Err(BundleError::Integrity {
            path: diagnostic,
            expected: expected_digest,
            actual,
        });
    }
    Ok(())
}

pub(crate) fn prepare_publish_parent(root: &Path) -> Result<PathBuf, BundleError> {
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    // A host may intentionally expose its temporary directory through a
    // compatibility symlink (for example macOS `/var`). Resolve that parent
    // once; the bundle target itself is still refused when it is a symlink.
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

pub(crate) fn reject_existing_target(root: &Path) -> Result<(), BundleError> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BundleError::ForbiddenSymlink {
            path: root.to_path_buf(),
        }),
        Ok(metadata) if !metadata.is_dir() => Err(BundleError::NotDirectory(root.to_path_buf())),
        Ok(metadata) if metadata.is_dir() => {
            if let Some(path) = find_symlink(root)? {
                Err(BundleError::ForbiddenSymlink { path })
            } else {
                Err(BundleError::TargetExists(root.to_path_buf()))
            }
        }
        Ok(_) => Err(BundleError::TargetExists(root.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BundleError::ReadFile {
            path: root.to_path_buf(),
            source,
        }),
    }
}

fn find_symlink(directory: &Path) -> Result<Option<PathBuf>, BundleError> {
    for entry in std::fs::read_dir(directory).map_err(|source| BundleError::ReadFile {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BundleError::ReadFile {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| BundleError::ReadFile {
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Ok(Some(path));
        }
        if metadata.is_dir()
            && let Some(path) = find_symlink(&path)?
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
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
                #[cfg(unix)]
                std::fs::set_permissions(
                    &staged,
                    std::os::unix::fs::PermissionsExt::from_mode(0o700),
                )
                .map_err(|source| BundleError::ReadFile {
                    path: staged.clone(),
                    source,
                })?;
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

#[cfg(unix)]
pub(crate) fn validate_layout(root: &BundleRoot, runtime: &Runtime) -> Result<(), BundleError> {
    let root_path = root.path();
    require_layout_directories(root)?;
    let allowed = [RUNTIME_FILE, ASSETS_DIR, BIN_DIR];
    for (name, kind) in unix::root_entries(root)? {
        let path = root_path.join(&name);
        if kind == unix::BundleEntryKind::Symlink {
            return Err(BundleError::ForbiddenSymlink { path });
        }
        let name = name
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?;
        if !allowed.contains(&name) {
            return Err(BundleError::UnexpectedFile { path });
        }
        if name == RUNTIME_FILE && kind != unix::BundleEntryKind::File {
            return Err(BundleError::UnsupportedEntry { path });
        }
        if name != RUNTIME_FILE && kind != unix::BundleEntryKind::Directory {
            return Err(BundleError::UnsupportedEntry { path });
        }
    }

    let mut expected_assets = BTreeMap::new();
    for entry in &runtime.assets.entries {
        expected_assets.insert(entry.path.clone(), entry);
        verify_file(root, entry)?;
    }
    let mut actual_assets = BTreeSet::new();
    let mut actual_asset_directories = BTreeSet::new();
    unix::collect_files(
        root,
        ASSETS_DIR,
        &mut actual_assets,
        &mut actual_asset_directories,
    )?;
    reject_unindexed_directories(
        root_path,
        &actual_asset_directories,
        &expected_assets.keys().cloned().collect::<BTreeSet<_>>(),
    )?;
    if actual_assets != expected_assets.keys().cloned().collect::<BTreeSet<_>>() {
        if let Some(path) = actual_assets
            .difference(&expected_assets.keys().cloned().collect())
            .next()
        {
            return Err(BundleError::UnexpectedFile {
                path: root_path.join(path.as_str()),
            });
        }
        if let Some(path) = expected_assets
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            .difference(&actual_assets)
            .next()
        {
            return Err(BundleError::MissingFile {
                path: root_path.join(path.as_str()),
            });
        }
    }

    let expected_binaries = runtime
        .artifacts
        .values()
        .map(|artifact| artifact.path.clone())
        .collect::<BTreeSet<_>>();
    let mut actual_binaries = BTreeSet::new();
    let mut actual_binary_directories = BTreeSet::new();
    unix::collect_files(
        root,
        BIN_DIR,
        &mut actual_binaries,
        &mut actual_binary_directories,
    )?;
    reject_unindexed_directories(root_path, &actual_binary_directories, &expected_binaries)?;
    if actual_binaries != expected_binaries {
        if let Some(path) = actual_binaries.difference(&expected_binaries).next() {
            return Err(BundleError::UnexpectedFile {
                path: root_path.join(path.as_str()),
            });
        }
        if let Some(path) = expected_binaries.difference(&actual_binaries).next() {
            return Err(BundleError::MissingFile {
                path: root_path.join(path.as_str()),
            });
        }
    }
    for artifact in runtime.artifacts.values() {
        verify_binary(root, artifact)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_layout(root: &BundleRoot, _runtime: &Runtime) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}

fn reject_unindexed_directories(
    root: &Path,
    actual: &BTreeSet<BundlePath>,
    files: &BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    let mut expected = BTreeSet::new();
    for file in files {
        let mut components = file.as_str().split('/').collect::<Vec<_>>();
        components.pop();
        for length in 1..=components.len() {
            expected.insert(BundlePath::new(components[..length].join("/"))?);
        }
    }
    if let Some(unindexed) = actual.difference(&expected).next() {
        return Err(BundleError::UnindexedDirectory {
            path: root.join(unindexed.as_str()),
        });
    }
    Ok(())
}

fn verify_binary(root: &BundleRoot, binary: &BinaryReference) -> Result<(), BundleError> {
    let path = binary.path.filesystem_path(root.path());
    let file = open_bundle_file(root, &binary.path)?;
    #[cfg(unix)]
    unix::verify_executable(&file, &path)?;
    verify_open_file(file, &path, binary.digest, Some(binary.size_bytes))
}

fn verify_file(root: &BundleRoot, entry: &AssetRecord) -> Result<(), BundleError> {
    verify_digest_and_size(root, &entry.path, entry.digest, Some(entry.size_bytes))
}

fn verify_digest_and_size(
    root: &BundleRoot,
    bundle_path: &BundlePath,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<(), BundleError> {
    let path = bundle_path.filesystem_path(root.path());
    let file = open_bundle_file(root, bundle_path)?;
    #[cfg(unix)]
    unix::verify_data_file(&file, &path)?;
    verify_open_file(file, &path, expected, expected_size)
}

fn verify_open_file(
    mut file: std::fs::File,
    path: &Path,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<(), BundleError> {
    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        });
    }
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| BundleError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| BundleError::Size {
                path: path.to_path_buf(),
                expected: expected_size.unwrap_or(u64::MAX),
                actual: u64::MAX,
            })?;
    }
    if let Some(expected_size) = expected_size
        && total != expected_size
    {
        return Err(BundleError::Size {
            path: path.to_path_buf(),
            expected: expected_size,
            actual: total,
        });
    }
    let actual = Sha256Digest(hasher.finalize().into());
    if actual != expected {
        return Err(BundleError::Integrity {
            path: path.to_path_buf(),
            expected,
            actual,
        });
    }
    Ok(())
}

pub(crate) fn read_and_verify(
    file: &mut std::fs::File,
    path: &Path,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<Vec<u8>, BundleError> {
    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        });
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    let actual_size = bytes.len() as u64;
    if let Some(expected_size) = expected_size
        && actual_size != expected_size
    {
        return Err(BundleError::Size {
            path: path.to_path_buf(),
            expected: expected_size,
            actual: actual_size,
        });
    }
    let actual = Sha256Digest::of(&bytes);
    if actual != expected {
        return Err(BundleError::Integrity {
            path: path.to_path_buf(),
            expected,
            actual,
        });
    }
    Ok(bytes)
}
