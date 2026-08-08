//! Winding a participant down: the one sequence every exit shares.

use std::time::Duration;

use crate::participant::api::Participant;
use crate::participant::managed::ManagedTasks;

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
    pub(crate) grace_ms: u64,
}

impl Teardown {
    pub(crate) async fn run<R>(self, participant: &R, api: &R::Api, state: &mut R::State)
    where
        R: Participant,
    {
        let Teardown {
            mut managed_tasks,
            grace_ms,
        } = self;

        let deadline = tokio::time::Instant::now() + Duration::from_millis(grace_ms);
        managed_tasks.cancel();

        // Bound the shutdown hook by the grace deadline: a hook that
        // parks/flushes hardware can hang, but the runner must still proceed to
        // bus close deterministically rather than leak the process. On timeout we
        // log and move on; the hook's task is dropped (cancelled at the next await).
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, participant.shutdown(api, state)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(target: "phoxal.runtime", error = %error, "shutdown hook returned error");
            }
            Err(_elapsed) => {
                tracing::warn!(
                    target: "phoxal.runtime",
                    grace_ms,
                    "shutdown hook exceeded the grace deadline; proceeding to bus close"
                );
            }
        }

        // Join managed tasks before the bus closes, on the same deadline the
        // hook already consumed part of.
        log_unjoined_managed_tasks(managed_tasks.join_until(deadline).await, grace_ms);
        tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");
    }
}

/// Clean up after a failed `Participant::setup`: cancel and join whatever the
/// participant already spawned, then hand back its own error.
///
/// The participant never reached the run loop, so nothing else will cancel
/// those tasks. Cleanup must not mask why setup failed, which is why the
/// original error is returned unchanged rather than replaced by anything that
/// goes wrong while joining.
pub(crate) async fn abandon_setup(
    managed_tasks: ManagedTasks,
    error: anyhow::Error,
    grace_ms: u64,
) -> anyhow::Error {
    let unjoined = managed_tasks
        .shutdown_within(Duration::from_millis(grace_ms))
        .await;
    log_unjoined_managed_tasks(unjoined, grace_ms);
    error
}

fn log_unjoined_managed_tasks(unjoined: Vec<String>, grace_ms: u64) {
    if !unjoined.is_empty() {
        tracing::warn!(
            target: "phoxal.runtime",
            tasks = ?unjoined,
            grace_ms,
            "managed tasks were still running at the shutdown grace deadline"
        );
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

    /// A hook that fails. Teardown must log it and keep going: the bus still has
    /// to close, and the participant's own failure is what gets reported.
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
        managed.spawn(name, ManagedTaskPolicy::FaultOnExit, async move {
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
            grace_ms: 150,
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
        Teardown {
            managed_tasks,
            grace_ms: 5_000,
        }
        .run(&FailingShutdown, &(), &mut state)
        .await;

        assert!(trace.called.load(Ordering::Relaxed), "the hook must run");
        assert!(
            cancelled.load(Ordering::Relaxed),
            "the work after the failing hook must still happen"
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
            5_000,
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
        Teardown {
            managed_tasks,
            grace_ms: 150,
        }
        .run(&HangingShutdown, &(), &mut state)
        .await;
        let elapsed = began.elapsed();

        assert!(
            cancelled.load(Ordering::Relaxed),
            "managed tasks must be cancelled even when the hook consumed the grace"
        );
        assert!(
            elapsed < Duration::from_millis(700),
            "one deadline covers the hook and the joining, took {elapsed:?}"
        );
    }
}
