//! Unix descriptor and no-replace publication primitives.

use std::path::Path;
use std::sync::Arc;

use crate::BundleError;

pub(super) fn open_root(requested: &Path) -> Result<Arc<std::os::fd::OwnedFd>, BundleError> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;

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
        return Err(super::root_open_error(requested));
    }
    Ok(Arc::new(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
}
