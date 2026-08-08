//! Pinned-root and filesystem entry points for bundle reads.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    ASSETS_DIR, BIN_DIR, BundleError, BundlePath, RUNTIME_FILE, RuntimeDocument, open_bundle_file,
    open_relative_directory, root_open_error,
};

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
            use std::ffi::CString;
            use std::os::fd::FromRawFd;

            let requested_c =
                CString::new(requested.as_os_str().as_encoded_bytes()).map_err(|_| {
                    BundleError::UnsupportedEntry {
                        path: requested.to_path_buf(),
                    }
                })?;
            let fd = unsafe {
                // SAFETY: the CString is NUL-free; flags pin one directory without following it.
                libc::open(
                    requested_c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(root_open_error(requested));
            }
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            Ok(Self {
                path: requested.to_path_buf(),
                fd: Arc::new(fd),
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

#[cfg(not(unix))]
pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}
