//! Unix descriptor and no-replace publication primitives.

use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{ASSETS_DIR, BIN_DIR, BundleError, BundlePath};

use super::BundleRoot;

pub(super) fn open_root(requested: &Path) -> Result<Arc<OwnedFd>, BundleError> {
    let requested_c = CString::new(requested.as_os_str().as_encoded_bytes()).map_err(|_| {
        BundleError::UnsupportedEntry {
            path: requested.to_path_buf(),
        }
    })?;
    let fd = unsafe {
        // SAFETY: a NUL-free pathname is opened once as a no-follow directory descriptor.
        libc::open(
            requested_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(root_open_error(requested));
    }
    Ok(Arc::new(unsafe { OwnedFd::from_raw_fd(fd) }))
}

pub(crate) fn open_executable_source(path: &Path) -> Result<std::fs::File, BundleError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let leaf = path
        .file_name()
        .ok_or_else(|| BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        })?;
    // Resolve compatibility symlinks in the caller's parent (for example
    // macOS /var -> /private/var) once, then pin every component from the
    // filesystem root. The leaf itself is never canonicalized or followed.
    let parent = parent
        .canonicalize()
        .map_err(|source| BundleError::ReadFile {
            path: parent.to_path_buf(),
            source,
        })?;
    let root = c"/";
    let root_fd = unsafe {
        // SAFETY: the static root path is NUL-free and opened as a directory.
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io_error_for_path(Path::new("/")));
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        let component =
            CString::new(component.as_bytes()).map_err(|_| BundleError::UnsupportedEntry {
                path: path.to_path_buf(),
            })?;
        let child = unsafe {
            // SAFETY: directory is pinned and component is one NUL-free name;
            // no-follow traversal prevents an ancestor swap from redirecting
            // the remaining walk.
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if child < 0 {
            return Err(io_error_for_path(path));
        }
        directory = unsafe { OwnedFd::from_raw_fd(child) };
    }
    let leaf = CString::new(leaf.as_bytes()).map_err(|_| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    let fd = unsafe {
        // SAFETY: the pinned parent descriptor and NUL-free leaf identify one
        // exact file; O_NOFOLLOW rejects a substituted leaf symlink.
        libc::openat(
            directory.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io_error_for_path(path));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        });
    }
    ensure_source_executable(&metadata, path)?;
    Ok(file)
}

pub(crate) fn ensure_staging_directory(
    root: &BundleRoot,
    relative: &str,
) -> Result<(), BundleError> {
    ensure_relative_directory(root, relative).map(|_| ())
}

pub(crate) fn create_staging_file(
    root: &BundleRoot,
    path: &BundlePath,
    mode: u32,
) -> Result<std::fs::File, BundleError> {
    let mut components = path.as_str().rsplitn(2, '/');
    let name = components
        .next()
        .ok_or_else(|| BundleError::UnsupportedEntry {
            path: path.filesystem_path(root.path()),
        })?;
    let parent = components.next().unwrap_or("");
    let parent_fd = ensure_relative_directory(root, parent)?;
    let name = CString::new(name).map_err(|_| BundleError::UnsupportedEntry {
        path: path.filesystem_path(root.path()),
    })?;
    let fd = unsafe {
        // SAFETY: the parent is an owned descriptor below the private staging
        // root and name is one normalized component. O_EXCL and O_NOFOLLOW
        // prevent leaf substitution from redirecting the write.
        libc::openat(
            parent_fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io_error_for_path(&path.filesystem_path(root.path())));
    }
    if unsafe { libc::fchmod(fd, mode as libc::mode_t) } != 0 {
        let source = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(BundleError::ReadFile {
            path: path.filesystem_path(root.path()),
            source,
        });
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub(crate) fn mark_staging_root_ready(root: &BundleRoot) -> Result<(), BundleError> {
    if unsafe { libc::fchmod(root.fd.as_raw_fd(), 0o755) } != 0 {
        return Err(BundleError::ReadFile {
            path: root.path().to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// Publish a staged directory without replacing a target created by a racing
/// writer.
pub(crate) fn publish_staging_root(staged: &Path, target: &Path) -> Result<(), BundleError> {
    let target_path = target.to_path_buf();
    let staged = CString::new(staged.as_os_str().as_encoded_bytes()).map_err(|_| {
        BundleError::UnsupportedEntry {
            path: staged.to_path_buf(),
        }
    })?;
    let target = CString::new(target.as_os_str().as_encoded_bytes()).map_err(|_| {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BundleEntryKind {
    Directory,
    File,
    Symlink,
    Unsupported,
}

pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    verify_root_mode(root)?;
    for directory in [ASSETS_DIR, BIN_DIR] {
        let path = root.path().join(directory);
        if open_relative_directory(root, directory, &path)?.is_none() {
            return Err(BundleError::MissingFile { path });
        }
    }
    Ok(())
}

pub(super) fn verify_root_mode(root: &BundleRoot) -> Result<(), BundleError> {
    verify_directory_mode(root.fd.as_raw_fd(), root.path())
}

pub(super) fn root_entries(
    root: &BundleRoot,
) -> Result<Vec<(std::ffi::OsString, BundleEntryKind)>, BundleError> {
    let root_path = root.path();
    let root_fd = duplicate_directory(root.fd.as_raw_fd(), root_path)?;
    list_directory(root_fd.as_raw_fd(), root_path)?
        .into_iter()
        .map(|name| {
            let path = root_path.join(&name);
            let kind = entry_kind(root_fd.as_raw_fd(), &name, &path)?;
            Ok((name, kind))
        })
        .collect()
}

pub(super) fn collect_files(
    root: &BundleRoot,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
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

fn collect_files_at(
    directory_fd: libc::c_int,
    directory_path: &Path,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
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

fn duplicate_directory(fd: libc::c_int, path: &Path) -> Result<OwnedFd, BundleError> {
    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn open_relative_directory(
    root: &BundleRoot,
    relative: &str,
    path: &Path,
) -> Result<Option<OwnedFd>, BundleError> {
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
        verify_directory_mode(parent.as_raw_fd(), path)?;
    }
    Ok(Some(parent))
}

fn verify_directory_mode(fd: libc::c_int, path: &Path) -> Result<(), BundleError> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } != 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let mode = unsafe { metadata.assume_init() }.st_mode;
    let actual = u64::from(mode & 0o777) as u32;
    if actual != 0o755 {
        return Err(BundleError::DirectoryMode {
            path: path.to_path_buf(),
            expected: 0o755,
            actual,
        });
    }
    Ok(())
}

fn ensure_relative_directory(root: &BundleRoot, relative: &str) -> Result<OwnedFd, BundleError> {
    let mut parent = duplicate_directory(root.fd.as_raw_fd(), root.path())?;
    if relative.is_empty() {
        return Ok(parent);
    }
    let mut diagnostic = root.path().to_path_buf();
    for component in relative.split('/') {
        diagnostic.push(component);
        let component = CString::new(component).map_err(|_| BundleError::UnsupportedEntry {
            path: diagnostic.clone(),
        })?;
        let created = unsafe {
            // SAFETY: parent is an owned directory descriptor and component
            // is one normalized name below the private staging root.
            libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o755)
        };
        if created != 0 {
            let source = std::io::Error::last_os_error();
            if source.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(BundleError::ReadFile {
                    path: diagnostic,
                    source,
                });
            }
        }
        let child = unsafe {
            // SAFETY: O_NOFOLLOW pins the created/existing child as a real
            // directory before the next component or leaf is touched.
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if child < 0 {
            return Err(io_error_for_path(&diagnostic));
        }
        if unsafe { libc::fchmod(child, 0o755) } != 0 {
            let source = std::io::Error::last_os_error();
            unsafe { libc::close(child) };
            return Err(BundleError::ReadFile {
                path: diagnostic,
                source,
            });
        }
        parent = unsafe { OwnedFd::from_raw_fd(child) };
    }
    Ok(parent)
}

fn open_directory_child(
    parent: libc::c_int,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<OwnedFd, BundleError> {
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
    let child = unsafe { OwnedFd::from_raw_fd(child) };
    verify_directory_mode(child.as_raw_fd(), path)?;
    Ok(child)
}

fn entry_kind(
    parent: libc::c_int,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<BundleEntryKind, BundleError> {
    use std::mem::MaybeUninit;

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

fn list_directory(fd: libc::c_int, path: &Path) -> Result<Vec<std::ffi::OsString>, BundleError> {
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

pub(crate) fn open_bundle_file(
    root: &BundleRoot,
    path: &BundlePath,
) -> Result<std::fs::File, BundleError> {
    let filesystem_path = path.filesystem_path(root.path());
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

pub(super) fn ensure_source_executable(
    metadata: &std::fs::Metadata,
    path: &Path,
) -> Result<(), BundleError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(BundleError::NotExecutable {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

pub(super) fn verify_executable(file: &std::fs::File, path: &Path) -> Result<(), BundleError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let actual = metadata.permissions().mode() & 0o777;
    if actual != 0o755 {
        return Err(BundleError::ExecutableMode {
            path: path.to_path_buf(),
            expected: 0o755,
            actual,
        });
    }
    Ok(())
}

pub(super) fn verify_data_file(file: &std::fs::File, path: &Path) -> Result<(), BundleError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let actual = metadata.permissions().mode() & 0o777;
    if actual != 0o644 {
        return Err(BundleError::DataFileMode {
            path: path.to_path_buf(),
            expected: 0o644,
            actual,
        });
    }
    Ok(())
}
