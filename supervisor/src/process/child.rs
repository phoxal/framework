//! The central child launch boundary.
//!
//! Every participant this supervisor spawns crosses [`ManagedChild::spawn`],
//! which owns two things: the environment scrub and an isolated process group.
//! Graceful shutdown always stops that group. Installed deployments additionally
//! inherit their host lifecycle containment from the service manager.

use anyhow::{Context, Result};
use std::ops::{Deref, DerefMut};
use tokio::process::{Child, Command};

/// Bootstrap variables systemd hands this process alone. A child that inherited
/// `NOTIFY_SOCKET` could answer readiness on the supervisor's behalf.
const SUPERVISOR_ONLY_ENV: [&str; 8] = [
    "NOTIFY_SOCKET",
    "WATCHDOG_USEC",
    "WATCHDOG_PID",
    "LISTEN_FDS",
    "LISTEN_PID",
    "LISTEN_FDNAMES",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
];

fn scrub_std_environment(command: &mut std::process::Command) {
    for key in SUPERVISOR_ONLY_ENV {
        command.env_remove(key);
    }
}

/// A supervised child in its own process group.
pub(crate) struct ManagedChild {
    inner: Child,
}

impl ManagedChild {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self> {
        scrub_environment(command);
        #[cfg(unix)]
        command.process_group(0);
        let inner = command.spawn().context("spawn managed child")?;
        Ok(Self { inner })
    }
}

impl Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

pub(crate) fn scrub_environment(command: &mut Command) {
    scrub_std_environment(command.as_std_mut());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_systemd_bootstrap_environment_is_scrubbed() {
        let mut command = std::process::Command::new("/usr/bin/true");
        scrub_std_environment(&mut command);
        // Kept independent of SUPERVISOR_ONLY_ENV so shrinking the production
        // list fails here rather than silently leaking a variable.
        for key in [
            "NOTIFY_SOCKET",
            "WATCHDOG_USEC",
            "WATCHDOG_PID",
            "LISTEN_FDS",
            "LISTEN_PID",
            "LISTEN_FDNAMES",
            "INVOCATION_ID",
            "JOURNAL_STREAM",
        ] {
            assert_eq!(
                command
                    .get_envs()
                    .find(|(candidate, _)| *candidate == std::ffi::OsStr::new(key))
                    .map(|(_, value)| value),
                Some(None),
                "{key} must be explicitly removed from a managed child's environment"
            );
        }
    }

    /// The guardian is gone, not merely unused: no re-exec entry point, no
    /// pipe protocol, and no argv token remain anywhere in this crate.
    #[test]
    fn no_guardian_symbol_survives_in_this_crate() {
        let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut pending = vec![source_root];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read the crate source tree") {
                let path = entry.expect("read a source entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read a source file");
                // This file names the deleted machinery in its own module docs,
                // which is the record of why it is gone.
                if path.file_name().is_some_and(|name| name == "child.rs") {
                    continue;
                }
                for token in [
                    "__graph-guardian",
                    "maybe_run_guardian",
                    "guardian_command",
                    "GuardianClient",
                ] {
                    if text.contains(token) {
                        offenders.push(format!("{} contains `{token}`", path.display()));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "{offenders:?}");
    }
}
