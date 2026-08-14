//! The central child launch boundary.
//!
//! Every participant this supervisor spawns crosses [`ManagedChild::spawn`],
//! which owns three things and nothing else: the environment scrub, the
//! isolated process group, and the kernel-level crash containment that outlives
//! this process.
//!
//! # Kernel crash containment
//!
//! - **Linux:** `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec`, so the kernel kills
//!   the child the moment this process dies.
//!   Installed deployments additionally run under a systemd unit, whose cgroup
//!   kill takes the whole tree down regardless.
//! - **macOS:** no equivalent exists. Every graceful path here still stops the
//!   child's whole process group explicitly, so an orderly shutdown - including
//!   the panic-free error paths - leaves nothing behind. The residual window is
//!   a *hard* kill of the supervisor (`SIGKILL`, a kernel panic) during a
//!   development run: participants spawned by that run can survive it. That is
//!   accepted for macOS development, where robots do not run in production.
//!
//! The trade this makes is deliberate: a guardian covered that last macOS
//! window at the cost of a second process, a hand-rolled record protocol, and a
//! spawn path that could fail in ways the thing it protected could not.

use anyhow::{Context, Result};
use std::ops::{Deref, DerefMut};
// Only the Linux containment hook needs `pre_exec`; elsewhere there is nothing
// to run between fork and exec.
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::unix::process::CommandExt as _;
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

/// Ask the kernel to kill this child when the spawning process dies.
///
/// Linux only, and best-effort by design: the call runs post-fork/pre-exec, so
/// it may only use async-signal-safe primitives, and a failure to arm it must
/// not fail an otherwise good spawn - it is a containment backstop, not the
/// shutdown path.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn arm_parent_death_signal(command: &mut std::process::Command) {
    // SAFETY: runs in the post-fork child before exec and calls only prctl,
    // which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn arm_parent_death_signal(_command: &mut std::process::Command) {
    // See this module's docs: macOS has no `PR_SET_PDEATHSIG`, so hard-killing
    // the supervisor on a development host can orphan participants. Graceful stops
    // signal the whole process group and are unaffected.
}

/// A supervised child in its own process group, contained by the kernel for as
/// long as this process lives.
pub(crate) struct ManagedChild {
    inner: Child,
}

impl ManagedChild {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self> {
        scrub_environment(command);
        #[cfg(unix)]
        command.process_group(0);
        arm_parent_death_signal(command.as_std_mut());
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
