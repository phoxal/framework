//! Bundle root resolution, staged writes, and verified reads.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::*;

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

/// One filesystem entry, classified without following a symlink.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
    /// Anything the bundle layout does not admit, including a symlink.
    Other,
}

impl From<std::fs::FileType> for EntryKind {
    fn from(value: std::fs::FileType) -> Self {
        if value.is_dir() {
            Self::Directory
        } else if value.is_file() {
            Self::File
        } else {
            Self::Other
        }
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

pub(crate) fn create_staging_file(
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
    let mut file = create_staging_file(root, path, DATA_MODE)?;
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
    let mut output = create_staging_file(root, destination, EXECUTABLE_MODE)?;
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

pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    for directory in [ASSETS_DIR, BIN_DIR] {
        let path = root.path().join(directory);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(BundleError::UnsupportedEntry { path }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(BundleError::MissingFile { path });
            }
            Err(source) => return Err(BundleError::ReadFile { path, source }),
        }
    }
    Ok(())
}

/// The layout admits exactly the indexed tree: `runtime.json`, the indexed
/// files below `assets/` and `bin/`, and the directories those files need.
pub(crate) fn validate_layout(root: &BundleRoot, runtime: &Runtime) -> Result<(), BundleError> {
    let root_path = root.path();
    require_layout_directories(root)?;
    let allowed = [RUNTIME_FILE, ASSETS_DIR, BIN_DIR];
    for (name, kind) in read_entries(root_path)? {
        let path = root_path.join(&name);
        let name = name
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?;
        if !allowed.contains(&name) {
            return Err(BundleError::UnexpectedFile { path });
        }
        let expected = if name == RUNTIME_FILE {
            EntryKind::File
        } else {
            EntryKind::Directory
        };
        if kind != expected {
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
    collect_files(
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
    collect_files(
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

fn read_entries(directory: &Path) -> Result<Vec<(std::ffi::OsString, EntryKind)>, BundleError> {
    let read = std::fs::read_dir(directory).map_err(|source| BundleError::ReadFile {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| BundleError::ReadFile {
            path: directory.to_path_buf(),
            source,
        })?;
        let kind = entry
            .file_type()
            .map_err(|source| BundleError::ReadFile {
                path: entry.path(),
                source,
            })?
            .into();
        entries.push((entry.file_name(), kind));
    }
    Ok(entries)
}

fn collect_files(
    root: &BundleRoot,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    let directory_path = root.path().join(relative_directory);
    collect_files_at(&directory_path, relative_directory, paths, directories)
}

fn collect_files_at(
    directory_path: &Path,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    for (name, kind) in read_entries(directory_path)? {
        let path = directory_path.join(&name);
        let name = name
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?;
        let relative = format!("{relative_directory}/{name}");
        match kind {
            EntryKind::Directory => {
                directories.insert(BundlePath::new(relative.clone())?);
                collect_files_at(&path, &relative, paths, directories)?;
            }
            EntryKind::File => {
                paths.insert(BundlePath::new(relative)?);
            }
            EntryKind::Other => return Err(BundleError::UnsupportedEntry { path }),
        }
    }
    Ok(())
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
    verify_digest_and_size(root, &binary.path, binary.digest, Some(binary.size_bytes))
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
