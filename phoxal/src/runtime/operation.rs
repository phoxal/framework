//! Bounded lifecycle for runner-owned finite operations.
//!
//! A managed operation owns at most one live worker and one pending latest
//! replacement.  Retiring an operation does not pretend that a detached
//! thread stopped: the worker remains charged until it exits and is joined.
//! A host that cannot confirm exit must terminate the owning process before it
//! can replace the resource.

use std::fmt;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::Activation;

/// A stable key selected by a runtime for one operation attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationKey(u64);

impl OperationKey {
    /// Creates an operation key.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the key value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Timeout and retirement bounds for one finite operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationPolicy {
    timeout: Duration,
    cancel_grace: Duration,
}

impl OperationPolicy {
    /// Creates policy values from positive millisecond bounds.
    pub const fn from_millis(
        timeout_ms: u64,
        cancel_grace_ms: u64,
    ) -> Result<Self, OperationError> {
        if timeout_ms == 0 {
            return Err(OperationError::InvalidPolicy("timeout_ms must be positive"));
        }
        if cancel_grace_ms == 0 {
            return Err(OperationError::InvalidPolicy(
                "cancel_grace_ms must be positive",
            ));
        }
        Ok(Self {
            timeout: Duration::from_millis(timeout_ms),
            cancel_grace: Duration::from_millis(cancel_grace_ms),
        })
    }

    /// Creates policy values from standard durations.
    pub const fn new(timeout: Duration, cancel_grace: Duration) -> Result<Self, OperationError> {
        if timeout.is_zero() {
            return Err(OperationError::InvalidPolicy("timeout must be positive"));
        }
        if cancel_grace.is_zero() {
            return Err(OperationError::InvalidPolicy(
                "cancel_grace must be positive",
            ));
        }
        Ok(Self {
            timeout,
            cancel_grace,
        })
    }

    /// Returns the worker execution deadline.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    /// Returns the non-extendable retirement grace period.
    #[must_use]
    pub const fn cancel_grace(self) -> Duration {
        self.cancel_grace
    }
}

/// Lifecycle state of a managed operation owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
    /// No worker is live or pending.
    Idle,
    /// One worker is executing.
    Running,
    /// A worker exceeded its deadline and must exit before replacement.
    Retiring,
}

/// Typed lifecycle failures from a managed operation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OperationError {
    /// A policy bound was zero.
    #[error("invalid operation policy: {0}")]
    InvalidPolicy(&'static str),
    /// The bounded pending replacement slot was replaced by a newer request.
    #[error("the pending operation was replaced by a newer activation")]
    PendingReplaced,
    /// The operation cannot be replaced until its owner exits and is joined.
    #[error("operation owner is still live and must be terminated before replacement")]
    OwnerMustTerminate,
    /// The operation was asked to retire while already idle.
    #[error("operation has no live worker")]
    NoWorker,
    /// A worker panicked.
    #[error("operation worker panicked")]
    Panicked,
}

/// A completion admitted from a managed operation owner.
pub struct OperationCompletion<Key, Response> {
    key: Key,
    outcome: OperationOutcome<Response>,
}

impl<Key, Response> OperationCompletion<Key, Response> {
    /// Creates a completion value.
    #[must_use]
    pub fn new(key: Key, outcome: OperationOutcome<Response>) -> Self {
        Self { key, outcome }
    }

    /// Returns the activation key.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the typed worker outcome.
    #[must_use]
    pub fn outcome(&self) -> &OperationOutcome<Response> {
        &self.outcome
    }

    /// Consumes the completion.
    #[must_use]
    pub fn into_parts(self) -> (Key, OperationOutcome<Response>) {
        (self.key, self.outcome)
    }
}

impl<Key: fmt::Debug, Response: fmt::Debug> fmt::Debug for OperationCompletion<Key, Response> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationCompletion")
            .field("key", &self.key)
            .field("outcome", &self.outcome)
            .finish()
    }
}

/// Result of one finite worker attempt.
pub enum OperationOutcome<Response> {
    /// The worker completed with an application result.
    Completed(crate::Result<Response>),
    /// The deadline elapsed before a result was admitted.
    TimedOut,
}

impl<Response: fmt::Debug> fmt::Debug for OperationOutcome<Response> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed(result) => formatter.debug_tuple("Completed").field(result).finish(),
            Self::TimedOut => formatter.write_str("TimedOut"),
        }
    }
}

/// A runner-owned finite operation with one live worker and one replacement
/// slot.
pub struct ManagedOperation<Key, Input, Response, Worker>
where
    Key: Clone + Send + 'static,
    Input: Send + 'static,
    Response: Send + 'static,
    Worker: Fn(Input) -> crate::Result<Response> + Send + Sync + 'static,
{
    policy: OperationPolicy,
    worker: std::sync::Arc<Worker>,
    live: Option<LiveWorker<Key, Response>>,
    pending: Option<Activation<Key, Input>>,
}

struct LiveWorker<Key, Response> {
    key: Key,
    receiver: Receiver<WorkerMessage<Response>>,
    handle: Option<JoinHandle<()>>,
    started: Instant,
    retiring_since: Option<Instant>,
    timed_out_reported: bool,
}

enum WorkerMessage<Response> {
    Finished(crate::Result<Response>),
    Panicked,
}

/// Result of submitting an activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitResult {
    /// The activation started immediately.
    Started,
    /// The activation occupied the one pending replacement slot.
    Pending,
    /// The old pending activation was replaced by this newer activation.
    ReplacedPending,
}

impl<Key, Input, Response, Worker> ManagedOperation<Key, Input, Response, Worker>
where
    Key: Clone + Send + 'static,
    Input: Send + 'static,
    Response: Send + 'static,
    Worker: Fn(Input) -> crate::Result<Response> + Send + Sync + 'static,
{
    /// Creates an idle managed operation.
    pub fn new(policy: OperationPolicy, worker: Worker) -> Self {
        Self {
            policy,
            worker: std::sync::Arc::new(worker),
            live: None,
            pending: None,
        }
    }

    /// Returns the current bounded lifecycle state.
    #[must_use]
    pub fn state(&self) -> OperationState {
        match &self.live {
            None => OperationState::Idle,
            Some(worker) if worker.retiring_since.is_some() => OperationState::Retiring,
            Some(_) => OperationState::Running,
        }
    }

    /// Reports whether the one pending replacement slot is occupied.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Submits an owned activation.  A live worker can have only one pending
    /// replacement; a newer replacement supersedes the older pending input.
    pub fn submit(
        &mut self,
        activation: Activation<Key, Input>,
    ) -> Result<SubmitResult, OperationError> {
        if self.live.is_none() {
            self.start(activation);
            return Ok(SubmitResult::Started);
        }
        if self.state() == OperationState::Retiring {
            return Err(OperationError::OwnerMustTerminate);
        }
        let result = if self.pending.replace(activation).is_some() {
            SubmitResult::ReplacedPending
        } else {
            SubmitResult::Pending
        };
        Ok(result)
    }

    /// Polls the worker without blocking the caller.
    pub fn poll(&mut self) -> Result<Option<OperationCompletion<Key, Response>>, OperationError> {
        let Some(worker) = self.live.as_mut() else {
            return Ok(None);
        };

        match worker.receiver.try_recv() {
            Ok(WorkerMessage::Finished(result)) => {
                let Some(mut worker) = self.live.take() else {
                    return Err(OperationError::NoWorker);
                };
                if let Some(handle) = worker.handle.take() {
                    handle.join().map_err(|_| OperationError::Panicked)?;
                }
                if worker.timed_out_reported {
                    // A timeout is already the admitted outcome.  Drain and
                    // join the late worker, but never expose a second result
                    // or let it become a fresh invocation input.
                    return Ok(None);
                }
                Ok(Some(OperationCompletion::new(
                    worker.key,
                    OperationOutcome::Completed(result),
                )))
            }
            Ok(WorkerMessage::Panicked) => {
                if worker.timed_out_reported {
                    let Some(mut worker) = self.live.take() else {
                        return Err(OperationError::NoWorker);
                    };
                    if let Some(handle) = worker.handle.take() {
                        handle.join().map_err(|_| OperationError::Panicked)?;
                    }
                    return Ok(None);
                }
                self.mark_retiring();
                Err(OperationError::Panicked)
            }
            Err(TryRecvError::Empty) => {
                if !worker.timed_out_reported && worker.started.elapsed() >= self.policy.timeout {
                    worker.timed_out_reported = true;
                    worker.retiring_since = Some(Instant::now());
                    let key = self.live.as_ref().map(|worker| worker.key.clone());
                    return key
                        .map(|key| {
                            Ok(Some(OperationCompletion::new(
                                key,
                                OperationOutcome::TimedOut,
                            )))
                        })
                        .unwrap_or(Err(OperationError::NoWorker));
                }
                if worker
                    .retiring_since
                    .is_some_and(|retired| retired.elapsed() >= self.policy.cancel_grace)
                {
                    return Err(OperationError::OwnerMustTerminate);
                }
                Ok(None)
            }
            Err(TryRecvError::Disconnected) => {
                self.mark_retiring();
                Err(OperationError::Panicked)
            }
        }
    }

    /// Starts the pending latest replacement after the live worker has exited.
    pub fn start_pending(&mut self) -> Result<bool, OperationError> {
        if self.live.is_some() {
            return Err(OperationError::OwnerMustTerminate);
        }
        let Some(activation) = self.pending.take() else {
            return Ok(false);
        };
        self.start(activation);
        Ok(true)
    }

    /// Retires a live worker and reports whether it has exited and been joined.
    pub fn retire(&mut self) -> Result<bool, OperationError> {
        let Some(worker) = self.live.as_mut() else {
            return Err(OperationError::NoWorker);
        };
        if worker.retiring_since.is_none() {
            worker.retiring_since = Some(Instant::now());
        }
        let finished = self
            .live
            .as_ref()
            .is_some_and(|worker| worker.handle.as_ref().is_some_and(JoinHandle::is_finished));
        if finished {
            let Some(mut worker) = self.live.take() else {
                return Ok(false);
            };
            if let Some(handle) = worker.handle.take() {
                handle.join().map_err(|_| OperationError::Panicked)?;
            }
            return Ok(true);
        }
        if self.live.as_ref().is_some_and(|worker| {
            worker
                .retiring_since
                .is_some_and(|start| start.elapsed() >= self.policy.cancel_grace)
        }) {
            return Err(OperationError::OwnerMustTerminate);
        }
        Ok(false)
    }

    /// Drops pending work and requires every live worker to exit before reset.
    pub fn reset(&mut self) -> Result<(), OperationError> {
        self.pending = None;
        if let Some(worker) = self.live.as_mut()
            && worker.retiring_since.is_none()
        {
            worker.retiring_since = Some(Instant::now());
        }
        let finished = self
            .live
            .as_ref()
            .is_some_and(|worker| worker.handle.as_ref().is_some_and(JoinHandle::is_finished));
        if finished {
            let Some(mut worker) = self.live.take() else {
                return Ok(());
            };
            if let Some(handle) = worker.handle.take() {
                handle.join().map_err(|_| OperationError::Panicked)?;
            }
            Ok(())
        } else if self.live.is_some() {
            Err(OperationError::OwnerMustTerminate)
        } else {
            Ok(())
        }
    }

    fn start(&mut self, activation: Activation<Key, Input>) {
        let (key, input) = activation.into_parts();
        let worker = std::sync::Arc::clone(&self.worker);
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker(input)));
            let message = match result {
                Ok(result) => WorkerMessage::Finished(result),
                Err(_) => WorkerMessage::Panicked,
            };
            let _ = sender.send(message);
        });
        self.live = Some(LiveWorker {
            key,
            receiver,
            handle: Some(handle),
            started: Instant::now(),
            retiring_since: None,
            timed_out_reported: false,
        });
    }

    fn mark_retiring(&mut self) {
        if let Some(worker) = self.live.as_mut() {
            worker.retiring_since.get_or_insert_with(Instant::now);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        Activation, ManagedOperation, OperationKey, OperationOutcome, OperationPolicy,
        OperationState, SubmitResult,
    };

    fn policy() -> OperationPolicy {
        OperationPolicy::new(Duration::from_secs(1), Duration::from_millis(100))
            .expect("valid policy")
    }

    #[test]
    fn one_live_worker_and_one_latest_pending_replacement() {
        let mut operation = ManagedOperation::new(policy(), |input: u64| Ok(input + 1));
        assert_eq!(
            operation
                .submit(Activation::new(OperationKey::new(1), 1))
                .expect("first worker"),
            SubmitResult::Started
        );
        assert_eq!(
            operation
                .submit(Activation::new(OperationKey::new(2), 2))
                .expect("first pending"),
            SubmitResult::Pending
        );
        assert_eq!(
            operation
                .submit(Activation::new(OperationKey::new(3), 3))
                .expect("replacement"),
            SubmitResult::ReplacedPending
        );
        assert!(operation.has_pending());
        while operation.poll().expect("poll").is_none() {
            std::thread::yield_now();
        }
        assert_eq!(operation.state(), OperationState::Idle);
        assert!(operation.start_pending().expect("start pending"));
        while operation.poll().expect("poll").is_none() {
            std::thread::yield_now();
        }
        assert_eq!(operation.state(), OperationState::Idle);
    }

    #[test]
    fn timeout_is_reported_without_detaching_the_worker() {
        let policy = OperationPolicy::new(Duration::from_millis(1), Duration::from_millis(1))
            .expect("valid policy");
        let mut operation = ManagedOperation::new(policy, |_input: u64| {
            std::thread::sleep(Duration::from_millis(20));
            Ok(7_u64)
        });
        operation
            .submit(Activation::new(OperationKey::new(1), 0))
            .expect("start");
        let completion = loop {
            if let Some(completion) = operation.poll().expect("poll") {
                break completion;
            }
            std::thread::yield_now();
        };
        assert!(matches!(completion.outcome(), OperationOutcome::TimedOut));
        assert!(
            operation
                .submit(Activation::new(OperationKey::new(2), 0))
                .is_err()
        );
    }

    #[test]
    fn late_completion_is_drained_without_a_second_outcome() {
        let policy = OperationPolicy::new(Duration::from_millis(1), Duration::from_millis(100))
            .expect("valid policy");
        let mut operation = ManagedOperation::new(policy, |_input: u64| {
            std::thread::sleep(Duration::from_millis(10));
            Ok(7_u64)
        });
        operation
            .submit(Activation::new(OperationKey::new(1), 0))
            .expect("start");
        let timeout = loop {
            if let Some(completion) = operation.poll().expect("poll") {
                break completion;
            }
            std::thread::yield_now();
        };
        assert!(matches!(timeout.outcome(), OperationOutcome::TimedOut));
        while operation.state() != OperationState::Idle {
            assert!(operation.poll().expect("drain").is_none());
            std::thread::yield_now();
        }
        assert!(operation.poll().expect("idle poll").is_none());
    }
}
