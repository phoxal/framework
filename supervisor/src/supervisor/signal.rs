//! Termination signals, routed into the one orderly stop the supervisor has.
//!
//! A termination signal is the same request as the API `stop` command, so it
//! must cancel the same token: the graph goes Stopping, every child is torn
//! down through the SIGTERM/grace/SIGKILL budget, the lifecycle reaches Stopped,
//! and the process exits 0. Nothing here records a failure, because being asked
//! to stop is not one.
//!
//! Without this, the supervisor dies under the default disposition of SIGTERM
//! and orphans the graph: participants run in their own process groups, so they
//! survive their supervisor and get reparented to init. Under systemd the
//! unit's `KillMode=control-group` would still reach them through the cgroup,
//! but the plain-SIGTERM cases - a `kill` in a development shell on macOS or
//! Linux, or a foreground `phoxal` client cleaning up the supervisor it owns -
//! have no such backstop and must be handled here.
//!
//! The handler comes from `ctrlc` with its `termination` feature, which covers
//! SIGINT, SIGTERM, and SIGHUP in one registration: SIGTERM is what a service
//! manager and a `kill` send, SIGINT is the interactive form of the same
//! request, and SIGHUP arrives when the terminal that launched a foreground
//! supervisor goes away - all three leave the graph with no owner, so all
//! three mean stop rather than continue headless.

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

/// Register the termination handler now, and cancel `shutdown` on the first
/// signal that arrives.
///
/// Registration is deliberately fallible and eager: a supervisor that cannot
/// install the handler has no orderly stop at all, which is a startup
/// precondition rather than a warning, and installing it before anything is
/// launched means a startup-time signal is caught rather than lethal.
///
/// A second signal while the stop is already in flight does nothing extra: the
/// token is already cancelled, and teardown is bounded by each participant's
/// shutdown grace and the SIGKILL escalation behind it, so there is nothing an
/// impatient repeat could usefully shorten. SIGKILL remains the operator's
/// escape hatch, as always.
pub(crate) fn cancel_on_termination(shutdown: CancellationToken) -> Result<()> {
    ctrlc::set_handler(move || {
        tracing::info!("termination signal received; stopping execution");
        shutdown.cancel();
    })
    .context("failed to install the termination signal handler")
}

/// SIGHUP stands in for the whole set: `ctrlc`'s `termination` feature
/// registers the three signals in one call, and it is the one signal no test
/// harness raises on its own, so the test binary cannot mistake a stray SIGTERM
/// for this one. The handler is installed before the signal is raised, so this
/// process catches it instead of dying under the default disposition.
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_termination_signal_cancels_the_token_the_api_stop_uses() {
        let shutdown = CancellationToken::new();
        cancel_on_termination(shutdown.clone()).expect("the handler installs");
        assert!(!shutdown.is_cancelled());

        // SAFETY: `raise` only enqueues a signal for this process and touches
        // no memory; the handler for it is installed above, so the default
        // terminating disposition does not apply.
        assert_eq!(
            unsafe { libc::raise(libc::SIGHUP) },
            0,
            "raising SIGHUP at this process must succeed"
        );

        tokio::time::timeout(Duration::from_secs(5), shutdown.cancelled())
            .await
            .expect("a termination signal must cancel the token");
    }
}
