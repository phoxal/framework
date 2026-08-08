//! Winding a participant down: the one sequence every exit shares.

use std::time::Duration;

use crate::participant::api::Participant;
use crate::participant::managed::ManagedTasks;

/// One absolute teardown deadline shared by the participant hook, managed
/// tasks, and bus close. Keeping the instant private prevents lifecycle code
/// from accidentally starting a fresh budget for a child stage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShutdownDeadline(tokio::time::Instant);

impl ShutdownDeadline {
    pub(crate) fn from_now(grace: Duration) -> Self {
        Self(tokio::time::Instant::now() + grace)
    }

    pub(crate) fn instant(self) -> tokio::time::Instant {
        self.0
    }

    pub(crate) fn remaining(self) -> Duration {
        self.0
            .saturating_duration_since(tokio::time::Instant::now())
    }
}

/// Evidence collected after the primary participant exit has been selected.
///
/// Cleanup is part of the terminal contract, not a best-effort logging side
/// channel: a clean run with failed cleanup is a failure, and a primary fault
/// carries this report alongside it.
#[derive(Debug, Default)]
pub(crate) struct TeardownReport {
    pub(crate) shutdown_error: Option<anyhow::Error>,
    pub(crate) shutdown_timed_out: bool,
    pub(crate) unjoined_tasks: Vec<String>,
    pub(crate) unjoined_error: Option<anyhow::Error>,
    pub(crate) task_errors: Vec<anyhow::Error>,
    pub(crate) bus_close_error: Option<anyhow::Error>,
    pub(crate) bus_close_report: Option<phoxal_bus::BusCloseReport>,
}

impl TeardownReport {
    pub(crate) fn is_clean(&self) -> bool {
        self.shutdown_error.is_none()
            && !self.shutdown_timed_out
            && self.unjoined_tasks.is_empty()
            && self.unjoined_error.is_none()
            && self.task_errors.is_empty()
            && self.bus_close_error.is_none()
            && self
                .bus_close_report
                .as_ref()
                .is_none_or(phoxal_bus::BusCloseReport::is_clean)
    }
}

impl std::fmt::Display for TeardownReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        let mut item = |label: &str, detail: &dyn std::fmt::Display| -> std::fmt::Result {
            if !first {
                write!(formatter, "; ")?;
            }
            first = false;
            write!(formatter, "{label}={detail}")
        };
        if let Some(error) = &self.shutdown_error {
            item("shutdown", error)?;
        }
        if self.shutdown_timed_out {
            item("shutdown-timeout", &"true")?;
        }
        if !self.unjoined_tasks.is_empty() {
            item("unjoined-tasks", &format_args!("{:?}", self.unjoined_tasks))?;
        }
        if !self.task_errors.is_empty() {
            let errors: Vec<_> = self.task_errors.iter().map(ToString::to_string).collect();
            item("task-errors", &format_args!("{errors:?}"))?;
        }
        if let Some(error) = &self.bus_close_error {
            item("bus-close", error)?;
        }
        if let Some(report) = &self.bus_close_report
            && !report.is_clean()
        {
            item("bus-close-report", report)?;
        }
        if first {
            formatter.write_str("clean")
        } else {
            Ok(())
        }
    }
}

/// The returned failure retains both the original runtime error and every
/// cleanup failure that followed it. `primary` remains the error source so
/// callers can still downcast the original fault through the error chain.
#[derive(Debug)]
pub(crate) struct TerminalError {
    pub(crate) primary: Option<anyhow::Error>,
    pub(crate) teardown: TeardownReport,
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.primary {
            Some(primary) => write!(formatter, "{primary}; teardown: {}", self.teardown),
            None => write!(formatter, "teardown failed: {}", self.teardown),
        }
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.primary
            .as_deref()
            .map(|error| error as _)
            .or_else(|| self.teardown.first_error())
    }
}

impl TeardownReport {
    fn first_error(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.shutdown_error
            .as_ref()
            .map(|error| error.as_ref() as _)
            .or_else(|| self.task_errors.first().map(|error| error.as_ref() as _))
            .or_else(|| {
                self.bus_close_error
                    .as_ref()
                    .map(|error| error.as_ref() as _)
            })
            .or_else(|| {
                self.unjoined_error
                    .as_ref()
                    .map(|error| error.as_ref() as _)
            })
            .or_else(|| {
                self.shutdown_timed_out
                    .then_some(&SHUTDOWN_TIMEOUT_ERROR as &(dyn std::error::Error + 'static))
            })
    }
}

static SHUTDOWN_TIMEOUT_ERROR: ShutdownTimeoutError = ShutdownTimeoutError;

#[derive(Debug)]
struct ShutdownTimeoutError;

impl std::fmt::Display for ShutdownTimeoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("participant shutdown hook exceeded its bounded grace")
    }
}

impl std::error::Error for ShutdownTimeoutError {}

pub(crate) fn combine<T>(primary: crate::Result<T>, teardown: TeardownReport) -> crate::Result<T> {
    if teardown.is_clean() {
        return primary;
    }
    Err(TerminalError {
        primary: primary.err(),
        teardown,
    }
    .into())
}

/// The shutdown sequence and the single grace budget that bounds it.
///
/// One type because the sequence and the deadline are inseparable: the
/// participant's hardware-safety hook and the joining of its managed tasks share
/// one deadline, so a stuck task cannot buy itself extra time after a slow hook,
/// and a hook that hangs cannot hold the process open. Every exit after
/// `Participant::setup` succeeds - normal completion, a loop fault, a failed
/// server or liveliness declaration - goes through here, so none of them can
/// bypass the hook or detach tasks before the bus closes.
pub(crate) struct Teardown {
    pub(crate) managed_tasks: ManagedTasks,
    pub(crate) deadline: ShutdownDeadline,
}

impl Teardown {
    pub(crate) async fn run<R>(
        self,
        participant: &R,
        api: &R::Api,
        state: &mut R::State,
    ) -> TeardownReport
    where
        R: Participant,
    {
        let Teardown {
            mut managed_tasks,
            deadline,
        } = self;
        let mut report = TeardownReport::default();

        // Bound the shutdown hook by the grace deadline: a hook that
        // parks/flushes hardware can hang, but the runner must still proceed to
        // bus close deterministically rather than leak the process. On timeout we
        // retain structured timeout evidence and move on; the hook's future is
        // dropped (cancelled at the next await).
        let remaining = deadline.remaining();
        match tokio::time::timeout(remaining, participant.shutdown(api, state)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                report.shutdown_error = Some(error);
            }
            Err(_elapsed) => {
                report.shutdown_timed_out = true;
            }
        }

        // Keep required I/O alive while the participant parks its hardware;
        // only after the hook returns or times out do we cancel and join the
        // runner-owned tasks, on the same deadline the hook consumed part of.
        managed_tasks.cancel();
        let task_report = managed_tasks.join_until(deadline.instant()).await;
        report.unjoined_tasks = task_report.unjoined;
        if !report.unjoined_tasks.is_empty() {
            report.unjoined_error = Some(anyhow::anyhow!(
                "managed tasks remained unjoined after bounded reaping: {:?}",
                report.unjoined_tasks
            ));
        }
        report.task_errors = task_report.failures;
        tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");
        report
    }
}

/// Clean up after a failed `Participant::setup`: cancel and join whatever the
/// participant already spawned, then hand back its own error.
///
/// The participant never reached the run loop, so nothing else will cancel
/// those tasks. Cleanup must not mask why setup failed, which is why the
/// original error remains the primary source while any join failures are
/// attached as structured teardown evidence.
pub(crate) async fn abandon_setup(
    mut managed_tasks: ManagedTasks,
    error: anyhow::Error,
    deadline: ShutdownDeadline,
) -> anyhow::Error {
    managed_tasks.cancel();
    let report = task_report(managed_tasks.join_until(deadline.instant()).await);
    if report.is_clean() {
        error
    } else {
        TerminalError {
            primary: Some(error),
            teardown: report,
        }
        .into()
    }
}

/// Cancel setup-owned work after a stop arrives before setup produced State/Api.
/// There is no participant shutdown hook to call yet, but task cleanup evidence
/// still participates in the terminal result.
pub(crate) async fn abandon_startup(
    mut managed_tasks: ManagedTasks,
    deadline: ShutdownDeadline,
) -> TeardownReport {
    managed_tasks.cancel();
    task_report(managed_tasks.join_until(deadline.instant()).await)
}

fn task_report(shutdown: crate::participant::managed::ManagedTaskShutdown) -> TeardownReport {
    let unjoined_error = (!shutdown.unjoined.is_empty()).then(|| {
        anyhow::anyhow!(
            "managed tasks remained unjoined after bounded reaping: {:?}",
            shutdown.unjoined
        )
    });
    TeardownReport {
        unjoined_tasks: shutdown.unjoined,
        unjoined_error,
        task_errors: shutdown.failures,
        ..TeardownReport::default()
    }
}

/// The runner runs [`Teardown`] unconditionally before it converts a loop exit
/// into the returned error, so "the participant still parks its hardware on the
/// way out" is a property of this sequence alone. Its composition with live
/// fault detection in the main loop needs a bus and is proven by the local
/// end-to-end run.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::participant::managed::ManagedTaskPolicy;
    use crate::prelude::*;
    use std::error::Error;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Per-instance so these tests stay independent under the parallel test
    /// harness; a shared static would make them race.
    #[derive(Clone, Default)]
    struct HookTrace {
        called: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    }

    /// A participant whose shutdown hook never returns, standing in for one
    /// that hangs parking or flushing hardware.
    #[phoxal::service(id = "hanging-shutdown", state = HookTrace)]
    struct HangingShutdown;

    impl Participant for HangingShutdown {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok((HookTrace::default(), ()))
        }

        async fn shutdown(&self, _api: &Self::Api, state: &mut Self::State) -> crate::Result<()> {
            state.called.store(true, Ordering::Relaxed);
            std::future::pending::<()>().await;
            state.completed.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    /// A hook that fails. Teardown records it and keeps going: the bus still has
    /// to close, and the participant's own failure remains the primary result.
    #[phoxal::service(id = "failing-shutdown", state = HookTrace)]
    struct FailingShutdown;

    impl Participant for FailingShutdown {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok((HookTrace::default(), ()))
        }

        async fn shutdown(&self, _api: &Self::Api, state: &mut Self::State) -> crate::Result<()> {
            state.called.store(true, Ordering::Relaxed);
            anyhow::bail!("could not park the wheels")
        }
    }

    /// One managed task that parks forever, already running, plus the flag its
    /// cancellation sets. Returns once the task is confirmed started, so a test
    /// asserting on cancellation cannot pass by racing it.
    async fn pending_managed_task(name: &str) -> (ManagedTasks, Arc<AtomicBool>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&cancelled);
        let started = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&started);

        let mut managed = ManagedTasks::default();
        managed.spawn(name, ManagedTaskPolicy::Critical, async move {
            struct OnCancel(Arc<AtomicBool>);
            impl Drop for OnCancel {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            let _guard = OnCancel(observed);
            running.store(true, Ordering::Relaxed);
            std::future::pending::<()>().await;
        });
        while !started.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
        (managed, cancelled)
    }

    /// A hook that hangs cannot hold the process open. Teardown gives up at the
    /// grace deadline and proceeds, leaving the hook cancelled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hanging_shutdown_hook_is_bounded_by_the_grace_deadline() {
        let trace = HookTrace::default();
        let mut state = trace.clone();

        let began = std::time::Instant::now();
        Teardown {
            managed_tasks: ManagedTasks::default(),
            deadline: ShutdownDeadline::from_now(Duration::from_millis(150)),
        }
        .run(&HangingShutdown, &(), &mut state)
        .await;
        let elapsed = began.elapsed();

        assert!(trace.called.load(Ordering::Relaxed), "the hook must run");
        assert!(
            elapsed < Duration::from_millis(700),
            "teardown must return at the grace deadline, took {elapsed:?}"
        );
        assert!(
            !trace.completed.load(Ordering::Relaxed),
            "the timed-out hook is dropped, not awaited to completion"
        );
    }

    /// A failing hook is not a reason to skip the rest of teardown: the managed
    /// tasks after it must still be cancelled and joined.
    #[tokio::test]
    async fn a_failing_shutdown_hook_does_not_abort_teardown() {
        let trace = HookTrace::default();
        let (managed_tasks, cancelled) = pending_managed_task("after-a-failing-hook").await;
        let mut state = trace.clone();

        // Returns at all, rather than propagating: teardown has no error path.
        let report = Teardown {
            managed_tasks,
            deadline: ShutdownDeadline::from_now(Duration::from_secs(5)),
        }
        .run(&FailingShutdown, &(), &mut state)
        .await;

        assert!(trace.called.load(Ordering::Relaxed), "the hook must run");
        assert!(
            cancelled.load(Ordering::Relaxed),
            "the work after the failing hook must still happen"
        );
        assert_eq!(
            report
                .shutdown_error
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("could not park the wheels"),
            "cleanup failures must remain structured evidence"
        );
    }

    /// The runner's own setup-failure cleanup: tasks spawned during
    /// `Participant::setup` are cancelled, and the participant's error survives
    /// the cleanup rather than being masked by it.
    #[tokio::test]
    async fn a_failed_setup_cancels_its_tasks_and_keeps_its_error() {
        let (managed, cancelled) = pending_managed_task("spawned-in-setup").await;

        let returned = abandon_setup(
            managed,
            anyhow::anyhow!("the serial port was not there"),
            ShutdownDeadline::from_now(Duration::from_secs(5)),
        )
        .await;

        assert!(
            cancelled.load(Ordering::Relaxed),
            "a task spawned before the failure must not outlive it"
        );
        assert_eq!(
            format!("{returned}"),
            "the serial port was not there",
            "cleanup must not mask why setup failed"
        );
    }

    /// The shutdown hook and managed-task joining share one deadline, so a
    /// managed task cannot buy itself extra time after a slow hook.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn managed_tasks_are_cancelled_and_joined_within_the_same_deadline() {
        let (managed_tasks, cancelled) = pending_managed_task("sensor-loop").await;

        let mut state = HookTrace::default();
        let began = std::time::Instant::now();
        let report = Teardown {
            managed_tasks,
            deadline: ShutdownDeadline::from_now(Duration::from_millis(150)),
        }
        .run(&HangingShutdown, &(), &mut state)
        .await;
        let elapsed = began.elapsed();

        assert!(
            cancelled.load(Ordering::Relaxed)
                || report.unjoined_tasks == vec!["sensor-loop".to_string()],
            "a task that cannot observe cancellation before the shared deadline must be reported"
        );
        assert!(
            elapsed < Duration::from_millis(700),
            "one deadline covers the hook and the joining, took {elapsed:?}"
        );
    }

    #[test]
    fn cleanup_failure_changes_a_clean_terminal_result_without_losing_primary_fault() {
        let clean = combine::<()>(
            Ok(()),
            TeardownReport {
                shutdown_error: Some(anyhow::anyhow!("park failed").context("shutdown hook")),
                ..TeardownReport::default()
            },
        )
        .expect_err("cleanup failure must fail an otherwise clean run");
        assert!(
            format!("{clean}").contains("shutdown=shutdown hook"),
            "unexpected cleanup rendering: {clean}"
        );
        let clean_terminal = clean
            .downcast_ref::<TerminalError>()
            .expect("cleanup failure must retain the terminal structure");
        assert!(
            clean_terminal.source().is_some(),
            "a clean-exit cleanup failure must expose an Error source"
        );
        assert!(
            clean_terminal
                .teardown
                .shutdown_error
                .as_ref()
                .is_some_and(|error| error.chain().count() >= 2),
            "cleanup source chains must remain inspectable"
        );

        let primary = anyhow::anyhow!("step failed").context("step transition");
        let combined = combine::<()>(
            Err(primary),
            TeardownReport {
                shutdown_error: Some(anyhow::anyhow!("park failed").context("shutdown hook")),
                ..TeardownReport::default()
            },
        )
        .expect_err("cleanup evidence must remain attached to a primary fault");
        assert!(format!("{combined}").contains("step transition"));
        let terminal = combined
            .downcast_ref::<TerminalError>()
            .expect("primary and cleanup must remain in a TerminalError");
        assert_eq!(
            terminal.primary.as_ref().map(ToString::to_string),
            Some("step transition".to_string())
        );
        assert!(terminal.teardown.shutdown_error.is_some());
        assert!(terminal.source().is_some());
    }

    #[test]
    fn bus_close_report_stays_structured_in_terminal_evidence() {
        let result = combine::<()>(
            Ok(()),
            TeardownReport {
                bus_close_report: Some(phoxal_bus::BusCloseReport {
                    transport_error_count: 3,
                    transport_errors: vec!["first failure".to_string()],
                    transport_errors_truncated: 2,
                    ..phoxal_bus::BusCloseReport::default()
                }),
                ..TeardownReport::default()
            },
        )
        .expect_err("transport close evidence must fail an otherwise clean run");
        let terminal = result
            .downcast_ref::<TerminalError>()
            .expect("close evidence must retain the terminal structure");
        let close = terminal
            .teardown
            .bus_close_report
            .as_ref()
            .expect("the structured close report must remain attached");
        assert_eq!(close.transport_error_count, 3);
        assert_eq!(close.transport_errors, ["first failure"]);
        assert!(format!("{result}").contains("3 transport failures"));
    }
}
