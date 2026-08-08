//! Pinned-root and filesystem entry points for bundle reads.

#[cfg(unix)]
mod unix;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
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

#[cfg(unix)]
pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    for directory in [ASSETS_DIR, BIN_DIR] {
        let path = root.path().join(directory);
        if open_relative_directory(root, directory, &path)?.is_none() {
            return Err(BundleError::MissingFile { path });
        }
    }
    Ok(())
}

pub(crate) fn write_new_file(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    ensure_staging_ancestors(root, path)?;
    let parent = path.parent().ok_or_else(|| BundleError::ReadFile {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no parent"),
    })?;
    ensure_staging_directory(root, parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path).map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    std::io::Write::write_all(&mut file, bytes).map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn copy_executable_source(
    root: &Path,
    source: &Path,
    destination: &Path,
    expected_digest: Sha256Digest,
    expected_size: u64,
) -> Result<(), BundleError> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let source_metadata =
        std::fs::symlink_metadata(source).map_err(|source_error| BundleError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: source.to_path_buf(),
        });
    }
    #[cfg(unix)]
    if source_metadata.permissions().mode() & 0o111 == 0 {
        return Err(BundleError::NotExecutable {
            path: source.to_path_buf(),
        });
    }

    ensure_staging_ancestors(root, destination)?;
    let mut input = std::fs::File::open(source).map_err(|source_error| BundleError::ReadFile {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source_error| BundleError::ReadFile {
            path: destination.to_path_buf(),
            source: source_error,
        })?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|source_error| BundleError::ReadFile {
                path: source.to_path_buf(),
                source: source_error,
            })?;
        if count == 0 {
            break;
        }
        std::io::Write::write_all(&mut output, &buffer[..count]).map_err(|source_error| {
            BundleError::ReadFile {
                path: destination.to_path_buf(),
                source: source_error,
            }
        })?;
        hasher.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| BundleError::Size {
                path: destination.to_path_buf(),
                expected: expected_size,
                actual: u64::MAX,
            })?;
    }
    let actual = Sha256Digest(hasher.finalize().into());
    if total != expected_size {
        return Err(BundleError::Size {
            path: destination.to_path_buf(),
            expected: expected_size,
            actual: total,
        });
    }
    if actual != expected_digest {
        return Err(BundleError::Integrity {
            path: destination.to_path_buf(),
            expected: expected_digest,
            actual,
        });
    }
    #[cfg(unix)]
    {
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).map_err(
            |source_error| BundleError::ReadFile {
                path: destination.to_path_buf(),
                source: source_error,
            },
        )?;
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
            Ok(()) => return Ok(staged),
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

pub(crate) fn ensure_staging_directory(root: &Path, directory: &Path) -> Result<(), BundleError> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| BundleError::UnsupportedEntry {
            path: directory.to_path_buf(),
        })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(BundleError::UnsupportedEntry {
                path: directory.to_path_buf(),
            });
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BundleError::ForbiddenSymlink { path: current });
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(BundleError::UnsupportedEntry { path: current }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|source| BundleError::ReadFile {
                    path: current.clone(),
                    source,
                })?;
            }
            Err(source) => {
                return Err(BundleError::ReadFile {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn ensure_staging_ancestors(root: &Path, path: &Path) -> Result<(), BundleError> {
    let parent = path.parent().ok_or_else(|| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    ensure_staging_directory(root, parent)
}

/// Publish the staged directory after the caller's advisory target check.
///
/// Another writer may create the target after `reject_existing_target` returns,
/// so this remains the security boundary and must use a kernel no-replace
/// primitive. It must never be changed to `std::fs::rename`, which replaces an
/// existing directory on POSIX and reintroduces the publication race.
pub(crate) fn publish_staging_root(staged: &Path, target: &Path) -> Result<(), BundleError> {
    let target_path = target.to_path_buf();
    let staged = std::ffi::CString::new(staged.as_os_str().as_encoded_bytes()).map_err(|_| {
        BundleError::UnsupportedEntry {
            path: staged.to_path_buf(),
        }
    })?;
    let target = std::ffi::CString::new(target.as_os_str().as_encoded_bytes()).map_err(|_| {
        BundleError::UnsupportedEntry {
            path: target.to_path_buf(),
        }
    })?;

    #[cfg(target_os = "linux")]
    {
        let result = unsafe {
            // SAFETY: both paths are NUL-free C strings. renameat2 performs
            // the destination existence check and rename as one syscall.
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                staged.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        map_no_replace_result(result, target_path)
    }

    #[cfg(target_os = "macos")]
    {
        let result = unsafe {
            // SAFETY: both paths are NUL-free C strings. renameatx_np with
            // RENAME_EXCL performs the destination existence check and rename
            // as one kernel operation.
            libc::renameatx_np(
                libc::AT_FDCWD,
                staged.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        map_no_replace_result(result as libc::c_long, target_path)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = staged;
        Err(BundleError::UnsupportedAtomicPublish { path: target_path })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn map_no_replace_result(result: libc::c_long, target: PathBuf) -> Result<(), BundleError> {
    if result == 0 {
        return Ok(());
    }
    let source = std::io::Error::last_os_error();
    match source.raw_os_error() {
        Some(libc::EEXIST) => Err(BundleError::TargetExists(target)),
        Some(libc::ENOSYS | libc::EINVAL | libc::ENOTSUP) => {
            Err(BundleError::UnsupportedAtomicPublish { path: target })
        }
        _ => Err(BundleError::ReadFile {
            path: target,
            source,
        }),
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BundleEntryKind {
    Directory,
    File,
    Symlink,
    Unsupported,
}

#[cfg(unix)]
pub(crate) fn validate_layout(root: &BundleRoot, runtime: &Runtime) -> Result<(), BundleError> {
    use std::os::fd::AsRawFd;

    let root_path = root.path();
    require_layout_directories(root)?;
    let root_fd = duplicate_directory(root.fd.as_raw_fd(), root_path)?;
    let allowed = [RUNTIME_FILE, ASSETS_DIR, BIN_DIR];
    for name in list_directory(root_fd.as_raw_fd(), root_path)? {
        let path = root_path.join(&name);
        let kind = entry_kind(root_fd.as_raw_fd(), &name, &path)?;
        if kind == BundleEntryKind::Symlink {
            return Err(BundleError::ForbiddenSymlink { path });
        }
        let name = name
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?;
        if !allowed.contains(&name) {
            return Err(BundleError::UnexpectedFile { path });
        }
        if name == RUNTIME_FILE && kind != BundleEntryKind::File {
            return Err(BundleError::UnsupportedEntry { path });
        }
        if name != RUNTIME_FILE && kind != BundleEntryKind::Directory {
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

#[cfg(not(unix))]
pub(crate) fn validate_layout(root: &BundleRoot, _runtime: &Runtime) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}

#[cfg(unix)]
fn collect_files(
    root: &BundleRoot,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    use std::os::fd::AsRawFd;

    let directory_path = root.path().join(relative_directory);
    let Some(directory_fd) = open_relative_directory(root, relative_directory, &directory_path)?
    else {
        return Ok(());
    };
    collect_files_at(
        directory_fd.as_raw_fd(),
        &directory_path,
        relative_directory,
        paths,
        directories,
    )
}

#[cfg(unix)]
fn collect_files_at(
    directory_fd: libc::c_int,
    directory_path: &Path,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    use std::os::fd::AsRawFd;

    for name in list_directory(directory_fd, directory_path)? {
        let path = directory_path.join(&name);
        let relative = relative_path(relative_directory, &name, &path)?;
        match entry_kind(directory_fd, &name, &path)? {
            BundleEntryKind::Symlink => return Err(BundleError::ForbiddenSymlink { path }),
            BundleEntryKind::Directory => {
                directories.insert(BundlePath::new(relative.clone())?);
                let child = open_directory_child(directory_fd, &name, &path)?;
                collect_files_at(child.as_raw_fd(), &path, &relative, paths, directories)?;
            }
            BundleEntryKind::File => {
                paths.insert(BundlePath::new(relative)?);
            }
            BundleEntryKind::Unsupported => return Err(BundleError::UnsupportedEntry { path }),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn relative_path(parent: &str, name: &std::ffi::OsStr, path: &Path) -> Result<String, BundleError> {
    let name = name.to_str().ok_or_else(|| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    Ok(if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    })
}

#[cfg(unix)]
fn duplicate_directory(fd: libc::c_int, path: &Path) -> Result<std::os::fd::OwnedFd, BundleError> {
    use std::os::fd::FromRawFd;

    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(unix)]
fn open_relative_directory(
    root: &BundleRoot,
    relative: &str,
    path: &Path,
) -> Result<Option<std::os::fd::OwnedFd>, BundleError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let mut parent = duplicate_directory(root.fd.as_raw_fd(), root.path())?;
    for component in relative.split('/') {
        let component = CString::new(component).map_err(|_| BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        })?;
        let child = unsafe {
            // SAFETY: parent is an owned directory descriptor and component
            // is one NUL-free relative name. O_NOFOLLOW prevents a directory
            // substitution from redirecting the walk.
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if child < 0 {
            let source = std::io::Error::last_os_error();
            if source.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            if source.raw_os_error() == Some(libc::ELOOP) || path_contains_symlink(path) {
                return Err(BundleError::ForbiddenSymlink {
                    path: path.to_path_buf(),
                });
            }
            return Err(BundleError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
        parent = unsafe { OwnedFd::from_raw_fd(child) };
    }
    Ok(Some(parent))
}

#[cfg(unix)]
fn open_directory_child(
    parent: libc::c_int,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<std::os::fd::OwnedFd, BundleError> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).map_err(|_| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    let child = unsafe {
        // SAFETY: parent is a directory descriptor and name is a NUL-free
        // single component. O_NOFOLLOW prevents a substituted child directory.
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if child < 0 {
        let source = std::io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ELOOP) {
            return Err(BundleError::ForbiddenSymlink {
                path: path.to_path_buf(),
            });
        }
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(unsafe { OwnedFd::from_raw_fd(child) })
}

#[cfg(unix)]
fn entry_kind(
    parent: libc::c_int,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<BundleEntryKind, BundleError> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).map_err(|_| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        // SAFETY: metadata points to writable storage and name is a NUL-free
        // relative entry. AT_SYMLINK_NOFOLLOW classifies the entry itself.
        libc::fstatat(
            parent,
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let metadata = unsafe { metadata.assume_init() };
    let mode = metadata.st_mode & libc::S_IFMT;
    if mode == libc::S_IFLNK {
        Ok(BundleEntryKind::Symlink)
    } else if mode == libc::S_IFDIR {
        Ok(BundleEntryKind::Directory)
    } else if mode == libc::S_IFREG {
        Ok(BundleEntryKind::File)
    } else {
        Ok(BundleEntryKind::Unsupported)
    }
}

#[cfg(unix)]
fn list_directory(fd: libc::c_int, path: &Path) -> Result<Vec<std::ffi::OsString>, BundleError> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStringExt;

    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        let source = std::io::Error::last_os_error();
        unsafe { libc::close(duplicate) };
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        });
    }

    let mut names = Vec::new();
    loop {
        reset_errno();
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            let source = std::io::Error::last_os_error();
            if source.raw_os_error().is_some_and(|error| error != 0) {
                unsafe { libc::closedir(directory) };
                return Err(BundleError::ReadFile {
                    path: path.to_path_buf(),
                    source,
                });
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(std::ffi::OsString::from_vec(name.to_bytes().to_vec()));
        }
    }
    if unsafe { libc::closedir(directory) } != 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(names)
}

#[cfg(unix)]
fn reset_errno() {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
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
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
            path: path.clone(),
            source,
        })?;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BundleError::NotExecutable { path });
        }
    }
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

pub(crate) fn open_bundle_file(
    root: &BundleRoot,
    path: &BundlePath,
) -> Result<std::fs::File, BundleError> {
    let filesystem_path = path.filesystem_path(root.path());
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let root_fd = unsafe { libc::dup(root.fd.as_raw_fd()) };
        if root_fd < 0 {
            return Err(BundleError::ReadFile {
                path: filesystem_path,
                source: std::io::Error::last_os_error(),
            });
        }
        let mut parent = unsafe { OwnedFd::from_raw_fd(root_fd) };
        let components = path.as_str().split('/').collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            let name = CString::new(*component).map_err(|_| BundleError::UnsupportedEntry {
                path: filesystem_path.clone(),
            })?;
            let flags = if index + 1 == components.len() {
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
            };
            let fd = unsafe {
                // SAFETY: parent remains an owned directory fd and name is a
                // NUL-free single path component. O_NOFOLLOW applies at every
                // component, preventing directory and leaf substitution.
                libc::openat(parent.as_raw_fd(), name.as_ptr(), flags)
            };
            if fd < 0 {
                return Err(io_error_for_path(&filesystem_path));
            }
            parent = unsafe { OwnedFd::from_raw_fd(fd) };
        }
        Ok(std::fs::File::from(parent))
    }
    #[cfg(not(unix))]
    {
        // No std-only API can bind every component to a no-follow directory
        // handle on these targets. Refuse the access rather than converting an
        // lstat/open check into a false security guarantee.
        Err(BundleError::UnsupportedSecureOpen {
            path: filesystem_path,
        })
    }
}

#[cfg(unix)]
fn root_open_error(path: &Path) -> BundleError {
    let source = std::io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ENOTDIR) {
        BundleError::NotDirectory(path.to_path_buf())
    } else if source.raw_os_error() == Some(libc::ELOOP) {
        BundleError::ForbiddenSymlink {
            path: path.to_path_buf(),
        }
    } else {
        BundleError::Root {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(unix)]
fn io_error_for_path(path: &Path) -> BundleError {
    let source = std::io::Error::last_os_error();
    if source.kind() == std::io::ErrorKind::NotFound {
        BundleError::MissingFile {
            path: path.to_path_buf(),
        }
    } else if source.raw_os_error() == Some(libc::ELOOP) || path_contains_symlink(path) {
        BundleError::ForbiddenSymlink {
            path: path.to_path_buf(),
        }
    } else {
        BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(unix)]
fn path_contains_symlink(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if std::fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(not(unix))]
pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}
