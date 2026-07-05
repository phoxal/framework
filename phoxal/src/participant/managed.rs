//! Runner-owned background tasks spawned during `#[setup]`
//! ([`SetupContext::spawn_managed`](crate::participant::context::SetupContext::spawn_managed)).
//!
//! A managed task is the framework-tracked alternative to a raw `tokio::spawn`
//! for long-lived work (sensor polling loops, serial/USB readers, async IO
//! pumps): the runner records it during `#[setup]`, watches for an unexpected
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
pub(crate) struct ManagedTaskExit {
    /// The task's diagnostic name.
    pub(crate) name: String,
    /// `Some(panic message)` if the task panicked; `None` if it returned
    /// normally.
    pub(crate) panic_message: Option<String>,
}

/// Registry `SetupContext` accumulates managed tasks into during `#[setup]`;
/// the runner takes ownership of it once `#[setup]` returns
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
