//! The host's "stop" request, as it reaches the runner.

use std::future::Future;

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

/// The two process signals that mean "stop", registered together.
///
/// A supervisor stops a child with SIGTERM and escalates to SIGKILL, so SIGTERM
/// is the signal the ordinary supervised stop arrives on; Ctrl-C (SIGINT) is the
/// interactive form of the same request. Both must reach the runner's teardown -
/// and therefore `Participant::shutdown` - so they are one seam, not two paths.
#[cfg(unix)]
struct TerminationSignals {
    interrupt: Signal,
    terminate: Signal,
}

#[cfg(unix)]
impl TerminationSignals {
    /// Install both handlers before either is awaited, so a signal arriving
    /// during startup is queued by the handler rather than killing the process
    /// under its default disposition.
    fn register() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    /// Resolve on the first SIGINT or SIGTERM, whichever arrives.
    async fn next(&mut self) {
        // `Signal::recv` is cancellation-safe, so losing the race costs no
        // delivery: the unselected stream keeps the signal queued.
        let received = tokio::select! {
            received = self.interrupt.recv() => received,
            received = self.terminate.recv() => received,
        };
        if received.is_none() {
            // `None` means the signal driver went away, not that anyone asked
            // for a stop. Resolving here would tear down a healthy participant.
            std::future::pending::<()>().await;
        }
    }
}

/// Register the host stop request before any supervised transport is opened.
///
/// Registration is deliberately fallible: a participant that cannot install
/// the SIGINT/SIGTERM handlers must not become Ready with a shutdown path that
/// can only be completed by SIGKILL.
#[cfg(unix)]
pub(crate) fn shutdown_signal() -> crate::Result<impl Future<Output = ()>> {
    let mut signals = TerminationSignals::register()?;
    Ok(async move { signals.next().await })
}

/// Resolve when the host asks this participant to stop.
///
/// Tokio's non-Unix Ctrl-C API has no synchronous registration constructor, so
/// the handler is installed when this returned future is first polled. The
/// Unix path above is eagerly registered before bus startup; non-Unix targets
/// therefore honestly provide Ctrl-C coverage once the runner reaches its
/// supervised loop, but cannot promise protection from a startup Ctrl-C during
/// an in-flight bus open.
#[cfg(not(unix))]
pub(crate) fn shutdown_signal() -> crate::Result<impl Future<Output = ()>> {
    Ok(async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(target: "phoxal.runtime", error = %e, "failed to listen for ctrl-c");
        }
    })
}

/// The signal coverage the supervised stop path depends on: the supervisor
/// sends SIGTERM, so SIGTERM must resolve the same shutdown trigger Ctrl-C
/// does. Both tests raise the signal at this process after registration
/// installed the handler, so the test binary catches it instead of dying.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The workspace panic gate exempts `#[test]` functions, but not a helper
    /// they share, so this one says so for itself.
    #[expect(
        clippy::expect_used,
        reason = "test support: a handler that will not install is the test's own failure to report"
    )]
    async fn resolves_on(signal: libc::c_int) {
        let mut signals = TerminationSignals::register().expect("both handlers install");
        // SAFETY: `raise` only enqueues a signal for this process and touches no
        // memory; the handler for it is installed above, so the default
        // terminating disposition does not apply.
        assert_eq!(
            unsafe { libc::raise(signal) },
            0,
            "raising signal {signal} at this process must succeed"
        );
        tokio::time::timeout(Duration::from_secs(5), signals.next())
            .await
            .unwrap_or_else(|_| panic!("signal {signal} must resolve the shutdown trigger"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial(process_signals)]
    async fn sigterm_triggers_shutdown() {
        resolves_on(libc::SIGTERM).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial(process_signals)]
    async fn sigint_triggers_shutdown() {
        resolves_on(libc::SIGINT).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial(process_signals)]
    async fn shutdown_future_registers_before_its_first_poll() {
        let shutdown = shutdown_signal().expect("signal handlers install");
        // If registration were deferred until polling the future, this signal
        // would still have the process-default terminating disposition.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .expect("a startup signal must be queued before the wait is polled");
    }
}
