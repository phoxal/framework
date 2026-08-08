//! Verification of the executable image used for a supervised participant.

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

use phoxal_bundle::Sha256Digest;

/// Hash the executable image that is actually running this participant.
///
/// Linux's `/proc/self/exe` is opened directly: if the bundle path is replaced
/// or unlinked after `execve`, the kernel still exposes the original executable
/// inode through that handle. Targets without an equivalent secure primitive
/// fail closed before transport startup; `current_exe()` is deliberately not
/// used because its path lookup is vulnerable to replacement between lookup
/// and open.
#[cfg(target_os = "linux")]
pub(crate) fn verify_current_executable(expected: Sha256Digest) -> crate::Result<()> {
    let (path, executable) = open_current_executable()?;
    let actual = Sha256Digest::from_reader(executable).map_err(|error| {
        anyhow::anyhow!(
            "failed to read running executable {}: {error}",
            path.display()
        )
    })?;
    if actual != expected {
        anyhow::bail!(
            "running executable {} does not match the runtime bundle binary digest (expected {}, got {})",
            path.display(),
            expected,
            actual
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn verify_current_executable(_expected: Sha256Digest) -> crate::Result<()> {
    Err(UnsupportedSecureExecutableIdentification.into())
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, thiserror::Error)]
#[error(
    "secure executable identification is unsupported on this target; supervised launch is disabled"
)]
pub(crate) struct UnsupportedSecureExecutableIdentification;

#[cfg(target_os = "linux")]
fn open_current_executable() -> crate::Result<(PathBuf, std::fs::File)> {
    let proc_path = Path::new("/proc/self/exe");
    let display_path = std::fs::read_link(proc_path).unwrap_or_else(|_| proc_path.into());
    let executable = std::fs::File::open(proc_path).map_err(|error| {
        anyhow::anyhow!(
            "failed to open the running executable {}: {error}",
            display_path.display()
        )
    })?;
    Ok((display_path, executable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_wrong_running_executable_digest_is_rejected_before_bus_open() {
        let error = verify_current_executable(Sha256Digest::of(b"not-this-process"))
            .expect_err("a substituted executable must fail local startup");
        let message = format!("{error:#}");
        assert!(message.contains("does not match the runtime bundle binary digest"));
        assert!(message.contains("expected"));
        assert!(message.contains("got"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_executable_identity_fails_before_bus_open() {
        let error = verify_current_executable(Sha256Digest::of(b"ignored"))
            .expect_err("supervised launch must fail closed on unsupported targets");
        assert!(
            error
                .downcast_ref::<UnsupportedSecureExecutableIdentification>()
                .is_some(),
            "the failure must identify the secure executable primitive as unsupported"
        );
        assert!(format!("{error:#}").contains("supervised launch is disabled"));
    }
}
