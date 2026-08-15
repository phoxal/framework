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
//! unit's default `KillMode=control-group` would still reach them through the
//! cgroup, but the plain-SIGTERM cases - a `kill` in a development shell on
//! macOS or Linux, or a foreground `phoxal` client cleaning up the supervisor
//! it owns - have no such backstop and must be handled here.

use std::future::Future;

use anyhow::{Context, Result};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// The three signals that mean "stop this execution", registered together.
///
/// SIGTERM is what a service manager and a `kill` send, SIGINT is the
/// interactive form of the same request, and SIGHUP arrives when the terminal
/// that launched a foreground supervisor goes away - all three leave the graph
/// with no owner, so all three mean stop rather than continue headless.
struct TerminationSignals {
    terminate: Signal,
    interrupt: Signal,
    hangup: Signal,
}

impl TerminationSignals {
    /// Install every handler before any of them is awaited, so a signal that
    /// arrives during startup is queued by the handler rather than killing the
    /// supervisor under its default disposition.
    fn register() -> Result<Self> {
        Ok(Self {
            terminate: install(SignalKind::terminate(), "SIGTERM")?,
            interrupt: install(SignalKind::interrupt(), "SIGINT")?,
            hangup: install(SignalKind::hangup(), "SIGHUP")?,
        })
    }

    /// Resolve with the name of the first signal to arrive.
    async fn next(&mut self) -> &'static str {
        // `Signal::recv` is cancellation-safe, so losing the race costs no
        // delivery: the unselected streams keep their signals queued.
        let (name, received) = tokio::select! {
            received = self.terminate.recv() => ("SIGTERM", received),
            received = self.interrupt.recv() => ("SIGINT", received),
            received = self.hangup.recv() => ("SIGHUP", received),
        };
        if received.is_none() {
            // `None` means the signal driver went away, not that anyone asked
            // for a stop. Resolving here would tear down a healthy execution.
            std::future::pending::<()>().await;
        }
        name
    }
}

fn install(kind: SignalKind, name: &str) -> Result<Signal> {
    signal(kind).with_context(|| format!("failed to install the {name} handler"))
}

/// Register the termination handlers now, and cancel `shutdown` on the first
/// signal that arrives.
///
/// Registration is deliberately fallible and eager: a supervisor that cannot
/// install these handlers has no orderly stop at all, which is a startup
/// precondition rather than a warning, and deferring registration to the first
/// poll would leave a startup-time SIGTERM lethal.
///
/// A second signal while the stop is already in flight does nothing extra: the
/// token is already cancelled, and teardown is bounded by each participant's
/// shutdown grace and the SIGKILL escalation behind it, so there is nothing an
/// impatient repeat could usefully shorten. SIGKILL remains the operator's
/// escape hatch, as always.
pub(crate) fn cancel_on_termination(
    shutdown: CancellationToken,
) -> Result<impl Future<Output = ()>> {
    let mut signals = TerminationSignals::register()?;
    Ok(async move {
        let name = signals.next().await;
        tracing::info!(
            signal = name,
            "termination signal received; stopping execution"
        );
        shutdown.cancel();
    })
}

/// SIGHUP stands in for the whole set: the three streams are registered by the
/// same call and select over the same seam, and it is the one signal no test
/// harness raises on its own, so the test binary cannot mistake a stray SIGTERM
/// for this one. The handler is installed before the signal is raised, so this
/// process catches it instead of dying under the default disposition.
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_termination_signal_cancels_the_token_the_api_stop_uses() {
        let shutdown = CancellationToken::new();
        let stopping = cancel_on_termination(shutdown.clone()).expect("all handlers install");
        assert!(!shutdown.is_cancelled());

        // SAFETY: `raise` only enqueues a signal for this process and touches
        // no memory; the handler for it is installed above, so the default
        // terminating disposition does not apply.
        assert_eq!(
            unsafe { libc::raise(libc::SIGHUP) },
            0,
            "raising SIGHUP at this process must succeed"
        );

        tokio::time::timeout(Duration::from_secs(5), stopping)
            .await
            .expect("a termination signal must resolve the stop");
        assert!(shutdown.is_cancelled());
    }
}
