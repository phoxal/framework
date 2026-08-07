//! Runner-owned background tasks spawned during `Participant::setup`
//! ([`SetupContext::spawn_managed`](crate::participant::context::SetupContext::spawn_managed)).
//!
//! A managed task is the framework-tracked alternative to a raw `tokio::spawn`
//! for long-lived work (sensor polling loops, serial/USB readers, async IO
//! pumps): the runner records it during `Participant::setup`, watches for an unexpected
//! exit or panic while the participant runs, and cancels + joins it during
//! shutdown before the bus closes. See [`ManagedTaskPolicy`] for what
//! "unexpected" means.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tokio::task::{Id, JoinError, JoinSet};
use tokio::time::Instant;

/// Whether a managed task ending on its own (without being cancelled by the
/// runner) is a runtime fault.
///
/// The runner only ever cancels a managed task during its shutdown sequence;
/// outside of that window, the task ending - by returning, panicking, or
/// otherwise stopping early - is always *unexpected* from the runner's point of
/// view. This policy just says how the runner should react to that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ManagedTaskPolicy {
    /// Ending on its own is a runtime fault: the runner logs the exit, marks
    /// the participant `Failed`, and stops.
    ///
    /// The default, and the right choice for anything meant to run for the
    /// participant's whole lifetime (sensor loops, IO pumps, watchdogs) - if the
    /// loop ever returns, the participant has silently lost that capability and
    /// should not keep reporting `Ready`.
    #[default]
    FaultOnExit,
    /// Ending on its own is expected and does not fault the participant.
    ///
    /// For one-shot setup-time work (a background warm-up, a best-effort cache
    /// prime) that legitimately finishes and returns. The runner treats any
    /// completion under this policy as non-fatal; a panic may still be reported by
    /// Tokio's panic hook, but it does not mark the participant `Failed`.
    AllowExit,
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
}

impl std::fmt::Display for ManagedTaskExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.panic_message {
            Some(message) => write!(
                formatter,
                "managed task \"{}\" panicked: {message}",
                self.name
            ),
            None => write!(
                formatter,
                "managed task \"{}\" exited unexpectedly",
                self.name
            ),
        }
    }
}

impl std::error::Error for ManagedTaskExit {}

/// Registry `SetupContext` accumulates managed tasks into during `Participant::setup`;
/// the runner takes ownership of it once `Participant::setup` returns
/// ([`SetupContext::take_managed_tasks`](crate::participant::context::SetupContext::take_managed_tasks))
/// and drives it from the main loop
/// ([`Self::next_unexpected_exit`]) and the shutdown sequence
/// ([`Self::shutdown_within`]).
#[derive(Default)]
pub(crate) struct ManagedTasks {
    join_set: JoinSet<()>,
    info: HashMap<Id, ManagedTaskInfo>,
}

impl ManagedTasks {
    /// Spawn `future` as a managed task named `name` under `policy`.
    pub(crate) fn spawn<F>(&mut self, name: impl Into<String>, policy: ManagedTaskPolicy, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let abort = self.join_set.spawn(future);
        self.info.insert(
            abort.id(),
            ManagedTaskInfo {
                name: name.into(),
                policy,
            },
        );
    }

    /// Wait for the next `FaultOnExit` managed task to end (return or panic),
    /// skipping over `AllowExit` completions. Pending forever once there are no
    /// `FaultOnExit` tasks left to watch, so callers can `select!` this
    /// alongside other branches without it ever winning a race spuriously.
    pub(crate) async fn next_unexpected_exit(&mut self) -> ManagedTaskExit {
        loop {
            let Some(result) = self.join_set.join_next_with_id().await else {
                return std::future::pending().await;
            };

            match result {
                Ok((id, ())) => {
                    let Some(info) = self.info.remove(&id) else {
                        continue;
                    };
                    if info.policy == ManagedTaskPolicy::AllowExit {
                        continue;
                    }
                    return ManagedTaskExit {
                        name: info.name,
                        panic_message: None,
                    };
                }
                Err(join_error) => {
                    let id = join_error.id();
                    let Some(info) = self.info.remove(&id) else {
                        continue;
                    };
                    if info.policy == ManagedTaskPolicy::AllowExit || join_error.is_cancelled() {
                        continue;
                    }
                    let panic_message = join_error
                        .is_panic()
                        .then(|| panic_message(join_error.into_panic()));
                    return ManagedTaskExit {
                        name: info.name,
                        panic_message,
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
    /// Returns the names of tasks still unjoined when `grace` elapsed.
    pub(crate) async fn shutdown_within(mut self, grace: Duration) -> Vec<String> {
        self.cancel();
        let deadline = Instant::now() + grace;
        self.join_until(deadline).await
    }

    /// Join remaining tasks until the shared shutdown deadline is exhausted.
    /// Returns the names of tasks still unjoined at `deadline`.
    pub(crate) async fn join_until(mut self, deadline: Instant) -> Vec<String> {
        loop {
            self.drain_ready();
            if self.info.is_empty() {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, self.join_set.join_next_with_id()).await {
                Ok(Some(result)) => self.forget_finished(result),
                Ok(None) => break,
                Err(_elapsed) => {
                    self.drain_ready();
                    break;
                }
            }
        }

        self.info.into_values().map(|info| info.name).collect()
    }

    fn drain_ready(&mut self) {
        while let Some(result) = self.join_set.try_join_next_with_id() {
            self.forget_finished(result);
        }
    }

    fn forget_finished(&mut self, result: Result<(Id, ()), JoinError>) {
        match result {
            Ok((id, ())) => {
                self.info.remove(&id);
            }
            Err(join_error) => {
                self.info.remove(&join_error.id());
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
        tasks.spawn("sensor-loop", ManagedTaskPolicy::FaultOnExit, async {});

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("a FaultOnExit task that returns must be reported");
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
        tasks.spawn("io-pump", ManagedTaskPolicy::FaultOnExit, async {
            panic!("serial port vanished");
        });

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("a panicking FaultOnExit task must be reported");
        assert_eq!(exit.name, "io-pump");
        assert_eq!(exit.panic_message.as_deref(), Some("serial port vanished"));
    }

    /// `AllowExit` is the whole point of the policy: neither a clean return nor
    /// a panic may fault the participant, and with nothing left to watch the
    /// future stays pending rather than resolving spuriously.
    #[tokio::test(start_paused = true)]
    async fn allow_exit_suppresses_both_a_return_and_a_panic() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("cache-prime", ManagedTaskPolicy::AllowExit, async {});
        tasks.spawn("warm-up", ManagedTaskPolicy::AllowExit, async {
            panic!("best-effort work failed");
        });

        assert!(
            tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
                .await
                .is_err(),
            "AllowExit completions must never surface as unexpected exits"
        );
    }

    /// A `FaultOnExit` task is still watched while `AllowExit` siblings come and
    /// go, so a real fault is not masked by the ones being skipped.
    #[tokio::test(start_paused = true)]
    async fn a_real_fault_is_not_masked_by_allow_exit_siblings() {
        let mut tasks = ManagedTasks::default();
        tasks.spawn("cache-prime", ManagedTaskPolicy::AllowExit, async {});
        tasks.spawn("watchdog", ManagedTaskPolicy::FaultOnExit, async {
            tokio::task::yield_now().await;
        });

        let exit = tokio::time::timeout(NEVER, tasks.next_unexpected_exit())
            .await
            .expect("the FaultOnExit task must still be reported");
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
        tasks.spawn("forever", ManagedTaskPolicy::FaultOnExit, async move {
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
            unjoined.is_empty(),
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
        tasks.spawn(
            "uncancellable",
            ManagedTaskPolicy::FaultOnExit,
            async move {
                running.store(true, Ordering::Relaxed);
                // Synchronous work cannot observe Tokio cancellation until it
                // returns to the scheduler. Kept finite so the test runtime still
                // shuts down promptly once the assertions below have run.
                std::thread::sleep(Duration::from_millis(1500));
                completed.store(true, Ordering::Relaxed);
            },
        );

        while !started.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }

        let began = std::time::Instant::now();
        let unjoined = tasks.shutdown_within(Duration::from_millis(150)).await;
        let elapsed = began.elapsed();

        assert_eq!(
            unjoined,
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
