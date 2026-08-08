//! Runner-owned background tasks spawned during `Participant::setup`
//! ([`SetupContext::spawn_managed`](crate::participant::context::SetupContext::spawn_managed)).
//!
//! A managed task is the framework-tracked alternative to a raw `tokio::spawn`
//! for long-lived work (sensor polling loops, serial/USB readers, async IO
//! pumps): the runner records it during `Participant::setup`, watches for an
//! unexpected exit or panic while the participant runs, and cancels + joins it
//! during shutdown before the bus closes. See [`ManagedTaskPolicy`] for what
//! "unexpected" means.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tokio::task::{Id, JoinError, JoinSet};
use tokio::time::Instant;

/// What completion a managed task promises to the participant runner.
///
/// Cancellation and joining are owned by the runner. Outside teardown, every
/// task completion is observed as a lifecycle event: a [`Critical`] task must
/// remain alive, while a [`Finite`] task is allowed to complete successfully.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ManagedTaskPolicy {
    /// A long-lived task whose unsolicited completion is a participant fault.
    /// A clean return, an error, a panic, or an unexpected cancellation all
    /// revoke the runner's ability to claim readiness.
    #[default]
    Critical,
    /// A setup-time or finite operation whose successful completion is normal.
    /// An error, panic, or unexpected cancellation remains a participant fault.
    Finite,
}

/// The output accepted by [`SetupContext::spawn_managed`](crate::SetupContext::spawn_managed).
///
/// `()` keeps long-lived loops concise. Returning `Result<(), E>` lets a task
/// carry an operational failure back to the runner instead of logging and
/// continuing. This trait is intentionally hidden from the authoring facade;
/// it is only the conversion seam for runner-owned task supervision.
#[doc(hidden)]
pub trait ManagedTaskOutput: Send + 'static {
    fn into_managed_result(self) -> anyhow::Result<()>;
}

impl ManagedTaskOutput for () {
    fn into_managed_result(self) -> anyhow::Result<()> {
        Ok(())
    }
}

impl<E> ManagedTaskOutput for Result<(), E>
where
    E: Into<anyhow::Error> + Send + 'static,
{
    fn into_managed_result(self) -> anyhow::Result<()> {
        self.map_err(Into::into)
    }
}

/// One managed task's diagnostic identity, recorded at spawn time and looked up
/// again by [`tokio::task::Id`] when the task ends.
struct ManagedTaskInfo {
    name: String,
    policy: ManagedTaskPolicy,
}

/// Why a managed task ended, for the runner's fault handling
/// ([`ManagedTasks::next_unexpected_exit`]).
///
/// This is the participant's failure itself, not a description of one: the
/// runner reports it directly, so the supervisor's failure evidence names the
/// task and what it did rather than a rendering the runner had to invent.
#[derive(Debug)]
pub(crate) struct ManagedTaskExit {
    /// The task's diagnostic name.
    pub(crate) name: String,
    /// `Some(panic message)` if the task panicked; `None` if it returned
    /// normally.
    pub(crate) panic_message: Option<String>,
    /// `Some(detail)` when the task returned an operational error.
    pub(crate) error_message: Option<String>,
}

impl std::fmt::Display for ManagedTaskExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.panic_message {
            Some(message) => write!(
                formatter,
                "managed task \"{}\" panicked: {message}",
                self.name
            ),
            None => match &self.error_message {
                Some(message) => write!(
                    formatter,
                    "managed task \"{}\" failed: {message}",
                    self.name
                ),
                None => write!(
                    formatter,
                    "managed task \"{}\" exited unexpectedly",
                    self.name
                ),
            },
        }
    }
}

impl std::error::Error for ManagedTaskExit {}

/// Join evidence collected during teardown. Cancellation is expected, while
/// an operational error or panic during cleanup is retained as evidence.
#[derive(Debug, Default)]
pub(crate) struct ManagedTaskShutdown {
    pub(crate) unjoined: Vec<String>,
    pub(crate) failures: Vec<String>,
}

/// Registry `SetupContext` accumulates managed tasks into during `Participant::setup`;
/// the runner takes ownership of it once `Participant::setup` returns
/// ([`SetupContext::take_managed_tasks`](crate::participant::context::SetupContext::take_managed_tasks))
/// and drives it from the main loop
/// ([`Self::next_unexpected_exit`]) and the shutdown sequence
/// ([`Self::shutdown_within`]).
#[derive(Default)]
pub(crate) struct ManagedTasks {
    join_set: JoinSet<anyhow::Result<()>>,
    info: HashMap<Id, ManagedTaskInfo>,
}

impl ManagedTasks {
    /// Spawn `future` as a managed task named `name` under `policy`.
    pub(crate) fn spawn<F>(&mut self, name: impl Into<String>, policy: ManagedTaskPolicy, future: F)
    where
        F: Future + Send + 'static,
        F::Output: ManagedTaskOutput,
    {
        let abort = self
            .join_set
            .spawn(async move { future.await.into_managed_result() });
        self.info.insert(
            abort.id(),
            ManagedTaskInfo {
                name: name.into(),
                policy,
            },
        );
    }

    /// Wait for the next policy violation, skipping successful `Finite`
    /// completions. Pending forever once there are no `Critical` tasks left to
    /// watch, so callers can `select!` this
    /// alongside other branches without it ever winning a race spuriously.
    pub(crate) async fn next_unexpected_exit(&mut self) -> ManagedTaskExit {
        loop {
            let Some(result) = self.join_set.join_next_with_id().await else {
                return std::future::pending().await;
            };

            match result {
                Ok((id, result)) => {
                    let Some(info) = self.info.remove(&id) else {
                        continue;
                    };
                    if info.policy == ManagedTaskPolicy::Finite && result.is_ok() {
                        continue;
                    }
                    return ManagedTaskExit {
                        name: info.name,
                        panic_message: None,
                        error_message: result.err().map(|error| error.to_string()),
                    };
                }
                Err(join_error) => {
                    let id = join_error.id();
                    let Some(info) = self.info.remove(&id) else {
                        continue;
                    };
                    let error_message = if join_error.is_cancelled() {
                        Some("cancelled unexpectedly".to_string())
                    } else {
                        None
                    };
                    let panic_message = if join_error.is_panic() {
                        Some(panic_message(join_error.into_panic()))
                    } else {
                        None
                    };
                    return ManagedTaskExit {
                        name: info.name,
                        panic_message,
                        error_message,
                    };
                }
            };
        }
    }

    /// Request cancellation for every remaining managed task.
    pub(crate) fn cancel(&mut self) {
        self.join_set.abort_all();
    }

    /// Cancel every remaining managed task and join them within `grace`.
    /// Returns unjoined task names and any non-cancellation join failures.
    pub(crate) async fn shutdown_within(mut self, grace: Duration) -> ManagedTaskShutdown {
        self.cancel();
        let deadline = Instant::now() + grace;
        self.join_until(deadline).await
    }

    /// Join remaining tasks until the shared shutdown deadline is exhausted.
    /// Returns unjoined task names and any non-cancellation join failures.
    pub(crate) async fn join_until(mut self, deadline: Instant) -> ManagedTaskShutdown {
        let mut failures = Vec::new();
        loop {
            self.drain_ready(&mut failures);
            if self.info.is_empty() {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, self.join_set.join_next_with_id()).await {
                Ok(Some(result)) => self.forget_finished(result, &mut failures),
                Ok(None) => break,
                Err(_elapsed) => {
                    self.drain_ready(&mut failures);
                    break;
                }
            }
        }

        let mut unjoined: Vec<_> = self.info.into_values().map(|info| info.name).collect();
        unjoined.sort();
        failures.sort();
        ManagedTaskShutdown { unjoined, failures }
    }

    fn drain_ready(&mut self, failures: &mut Vec<String>) {
        while let Some(result) = self.join_set.try_join_next_with_id() {
            self.forget_finished(result, failures);
        }
    }

    fn forget_finished(
        &mut self,
        result: Result<(Id, anyhow::Result<()>), JoinError>,
        failures: &mut Vec<String>,
    ) {
        match result {
            Ok((id, task_result)) => {
                if let Some(info) = self.info.remove(&id)
                    && let Err(error) = task_result
                {
                    failures.push(format!("{}: {error}", info.name));
                }
            }
            Err(join_error) => {
                if let Some(info) = self.info.remove(&join_error.id())
                    && !join_error.is_cancelled()
                {
                    let detail = if join_error.is_panic() {
                        panic_message(join_error.into_panic())
                    } else {
                        join_error.to_string()
                    };
                    failures.push(format!("{}: {detail}", info.name));
                }
            }
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "managed task panicked with a non-string payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{ManagedTaskPolicy, ManagedTasks};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Long enough that a correct implementation never reaches it, and
    /// instantaneous under a paused clock.
    const NEVER: Duration = Duration::from_secs(3600);

    /// A task ending on its own under the default policy is a fault, and the
    /// runner learns which task it was.
    #[tokio::test(start_paused = true)]
    async fn an_early_return_faults_and_names_the_task() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("sensor-loop", ManagedTaskPolicy::Critical, async {});

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("a Critical task that returns must be reported");
        assert_eq!(exit.name, "sensor-loop");
        assert_eq!(
            exit.panic_message, None,
            "a normal return is an unexpected exit, not a panic"
        );
    }

    /// A panic is reported with its message, so the participant's failure says
    /// what actually broke.
    #[tokio::test(start_paused = true)]
    async fn a_panic_is_reported_with_its_message() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("io-pump", ManagedTaskPolicy::Critical, async {
            panic!("serial port vanished");
            #[allow(unreachable_code)]
            ()
        });

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("a panicking Critical task must be reported");
        assert_eq!(exit.name, "io-pump");
        assert_eq!(exit.panic_message.as_deref(), Some("serial port vanished"));
    }

    /// A successful `Finite` return is expected, while a panic still faults the
    /// participant. A finite task is not silently detached merely because it is
    /// allowed to complete.
    #[tokio::test(start_paused = true)]
    async fn finite_success_is_allowed_but_panic_is_a_fault() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("cache-prime", ManagedTaskPolicy::Finite, async {});
        tasks.spawn("warm-up", ManagedTaskPolicy::Finite, async {
            panic!("best-effort work failed");
            #[allow(unreachable_code)]
            ()
        });

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("a finite panic must surface as a task fault");
        assert_eq!(exit.name, "warm-up");
        assert_eq!(
            exit.panic_message.as_deref(),
            Some("best-effort work failed")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn finite_error_is_a_fault_with_operational_detail() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("cache-prime", ManagedTaskPolicy::Finite, async {
            Err::<(), _>(anyhow::anyhow!("cache index is corrupt"))
        });

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("a finite operational error must surface as a task fault");
        assert_eq!(
            exit.error_message.as_deref(),
            Some("cache index is corrupt")
        );
        assert_eq!(
            format!("{exit}"),
            "managed task \"cache-prime\" failed: cache index is corrupt"
        );
    }

    /// A `Critical` task is still watched while `Finite` siblings complete, so a
    /// real fault is not masked by successful finite work.
    #[tokio::test(start_paused = true)]
    async fn a_real_fault_is_not_masked_by_finite_siblings() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("cache-prime", ManagedTaskPolicy::Finite, async {});
        tasks.spawn("watchdog", ManagedTaskPolicy::Critical, async {
            tokio::task::yield_now().await;
        });

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("the Critical task must still be reported");
        assert_eq!(exit.name, "watchdog");
    }

    /// Shutdown cancels every remaining task and joins it, reporting nothing
    /// unjoined. Cancellation is observable by the task itself.
    #[tokio::test]
    async fn shutdown_cancels_and_joins_every_task() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&cancelled);
        let started = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&started);

        let mut tasks = ManagedTasks::default();
        tasks.spawn("forever", ManagedTaskPolicy::Critical, async move {
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

        let unjoined = tasks.shutdown_within(Duration::from_secs(5)).await;
        assert!(
            unjoined.unjoined.is_empty(),
            "a cancellable task must join: {unjoined:?}"
        );
        assert!(
            cancelled.load(Ordering::Relaxed),
            "the task must observe cancellation"
        );
    }

    /// A task inside a synchronous section cannot observe cancellation, so it
    /// must not be allowed to extend the shutdown deadline: the grace elapses,
    /// the task is reported by name, and shutdown returns anyway.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_uncancellable_task_is_reported_rather_than_waited_for() {
        let started = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&started);
        let finished = Arc::new(AtomicBool::new(false));
        let completed = Arc::clone(&finished);

        let mut tasks = ManagedTasks::default();
        tasks.spawn("uncancellable", ManagedTaskPolicy::Critical, async move {
            running.store(true, Ordering::Relaxed);
            // Synchronous work cannot observe Tokio cancellation until it
            // returns to the scheduler. Kept finite so the test runtime still
            // shuts down promptly once the assertions below have run.
            std::thread::sleep(Duration::from_millis(1500));
            completed.store(true, Ordering::Relaxed);
        });

        while !started.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }

        let began = std::time::Instant::now();
        let unjoined = tasks.shutdown_within(Duration::from_millis(150)).await;
        let elapsed = began.elapsed();

        assert_eq!(
            unjoined.unjoined,
            vec!["uncancellable".to_string()],
            "a task still running at the grace deadline is reported by name"
        );
        assert!(
            elapsed < Duration::from_millis(700),
            "shutdown must be bounded by the grace budget, took {elapsed:?}"
        );
        assert!(
            !finished.load(Ordering::Relaxed),
            "shutdown must return before a cancellation-ignoring task finishes"
        );
    }
}
