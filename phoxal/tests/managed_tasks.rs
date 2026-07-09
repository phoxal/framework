//! `SetupContext::spawn_managed` integration: the runner watches managed tasks
//! for an unexpected exit/panic, faults the participant by default
//! (`ManagedTaskPolicy::FaultOnExit`), leaves `AllowExit` tasks alone, and
//! cancels + joins every managed task at shutdown within the grace budget.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phoxal::participant::{ManagedTaskPolicy, ParticipantLaunch, RealClock, run_with};
use phoxal::prelude::*;

static NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_namespace(label: &str) -> String {
    let seq = NAMESPACE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("test/managed/{label}/{}/{}", std::process::id(), seq)
}

fn local_launch(label: &str, participant_id: &str) -> ParticipantLaunch {
    let mut launch = ParticipantLaunch::local(participant_id, "robot");
    launch.namespace = unique_namespace(label);
    launch
}

// -- Panic: a `FaultOnExit` managed task panicking faults the runtime. --

static PANIC_TASK_STARTED: AtomicBool = AtomicBool::new(false);

#[phoxal::service(id = "managed-panic", config = (), api = ())]
struct ManagedPanic;

#[phoxal::behavior]
impl ManagedPanic {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed("panicking-task", async move {
            PANIC_TASK_STARTED.store(true, Ordering::Relaxed);
            panic!("managed task deliberately panics");
        });
        Ok((Self, ()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_task_panic_faults_the_runtime() {
    let launch = local_launch("panic", "managed-panic-1");
    // No explicit shutdown trigger: the fault itself must end the run loop.
    let shutdown = std::future::pending();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_with::<ManagedPanic, _, _>(launch, RealClock::new(), shutdown),
    )
    .await
    .expect("runner must not hang after a managed task panics");

    assert!(
        PANIC_TASK_STARTED.load(Ordering::Relaxed),
        "the managed task should have started before panicking"
    );
    let err = result.expect_err("a panicking FaultOnExit managed task should fault the runtime");
    assert!(
        err.to_string().contains("panicking-task"),
        "fault error should name the task: {err}"
    );
}

// -- Early return, FaultOnExit (default): faults the runtime. --

static EARLY_RETURN_FAULT_TASK_RAN: AtomicBool = AtomicBool::new(false);

#[phoxal::service(id = "managed-early-return-fault", config = (), api = ())]
struct ManagedEarlyReturnFault;

#[phoxal::behavior]
impl ManagedEarlyReturnFault {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed("early-return-task", async move {
            EARLY_RETURN_FAULT_TASK_RAN.store(true, Ordering::Relaxed);
            // Returns immediately: under the default FaultOnExit policy this is
            // just as unexpected as a panic.
        });
        Ok((Self, ()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_task_early_return_faults_by_default() {
    let launch = local_launch("early-return-fault", "managed-early-return-fault-1");
    let shutdown = std::future::pending();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_with::<ManagedEarlyReturnFault, _, _>(launch, RealClock::new(), shutdown),
    )
    .await
    .expect("runner must not hang after a managed task returns early");

    assert!(EARLY_RETURN_FAULT_TASK_RAN.load(Ordering::Relaxed));
    let err = result.expect_err("an early-returning FaultOnExit managed task should fault");
    assert!(
        err.to_string().contains("early-return-task"),
        "fault error should name the task: {err}"
    );
}

// -- Early return, AllowExit: does NOT fault the runtime. --

static EARLY_RETURN_ALLOWED_TASK_RAN: AtomicBool = AtomicBool::new(false);
static ALLOWED_RUNNER_STEPPED: AtomicU64 = AtomicU64::new(0);

#[phoxal::service(id = "managed-early-return-allowed", config = (), api = ())]
struct ManagedEarlyReturnAllowed;

#[phoxal::behavior]
impl ManagedEarlyReturnAllowed {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed_with("one-shot-init", ManagedTaskPolicy::AllowExit, async move {
            EARLY_RETURN_ALLOWED_TASK_RAN.store(true, Ordering::Relaxed);
        });
        Ok((Self, ()))
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let _ = step.time();
        ALLOWED_RUNNER_STEPPED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_task_early_return_under_allow_exit_does_not_fault() {
    let launch = local_launch("early-return-allowed", "managed-early-return-allowed-1");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let result =
        run_with::<ManagedEarlyReturnAllowed, _, _>(launch, RealClock::new(), shutdown).await;

    assert!(EARLY_RETURN_ALLOWED_TASK_RAN.load(Ordering::Relaxed));
    result.expect("an AllowExit managed task returning early must not fault the runtime");
    assert!(
        ALLOWED_RUNNER_STEPPED.load(Ordering::Relaxed) > 0,
        "the runner should keep stepping normally after the AllowExit task finished"
    );
}

// -- Panic, AllowExit: does NOT fault the runtime. --

static ALLOWED_PANIC_TASK_RAN: AtomicBool = AtomicBool::new(false);
static ALLOWED_PANIC_RUNNER_STEPPED: AtomicU64 = AtomicU64::new(0);

#[phoxal::service(id = "managed-panic-allowed", config = (), api = ())]
struct ManagedPanicAllowed;

#[phoxal::behavior]
impl ManagedPanicAllowed {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed_with("allowed-panic", ManagedTaskPolicy::AllowExit, async move {
            ALLOWED_PANIC_TASK_RAN.store(true, Ordering::Relaxed);
            panic!("allowed managed task deliberately panics");
        });
        Ok((Self, ()))
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let _ = step.time();
        ALLOWED_PANIC_RUNNER_STEPPED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_task_panic_under_allow_exit_does_not_fault() {
    let launch = local_launch("panic-allowed", "managed-panic-allowed-1");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    let result = run_with::<ManagedPanicAllowed, _, _>(launch, RealClock::new(), shutdown).await;

    assert!(ALLOWED_PANIC_TASK_RAN.load(Ordering::Relaxed));
    result.expect("an AllowExit managed task panicking must not fault the runtime");
    assert!(
        ALLOWED_PANIC_RUNNER_STEPPED.load(Ordering::Relaxed) > 0,
        "the runner should keep stepping normally after the AllowExit task panicked"
    );
}

// -- Clean shutdown: a well-behaved managed task is cancelled and joined. --

static COOPERATIVE_TASK_STARTED: AtomicBool = AtomicBool::new(false);
static COOPERATIVE_TASK_CANCELLED: AtomicBool = AtomicBool::new(false);

#[phoxal::service(id = "managed-clean-shutdown", config = (), api = ())]
struct ManagedCleanShutdown;

#[phoxal::behavior]
impl ManagedCleanShutdown {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed("cooperative-loop", async move {
            COOPERATIVE_TASK_STARTED.store(true, Ordering::Relaxed);
            // Parks forever until the runner cancels it at shutdown; a
            // dropped-at-cancellation guard flips the flag on unwind.
            let _guard = SetFlagOnDrop(&COOPERATIVE_TASK_CANCELLED);
            std::future::pending::<()>().await;
        });
        Ok((Self, ()))
    }
}

struct SetFlagOnDrop(&'static AtomicBool);

impl Drop for SetFlagOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_shutdown_cancels_and_joins_managed_tasks() {
    let launch = local_launch("clean-shutdown", "managed-clean-shutdown-1");
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
    };

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_with::<ManagedCleanShutdown, _, _>(launch, RealClock::new(), shutdown),
    )
    .await
    .expect("runner must not hang joining a cooperative managed task");

    result.expect("a cleanly cancelled managed task must not fault the runtime");
    assert!(COOPERATIVE_TASK_STARTED.load(Ordering::Relaxed));
    assert!(
        COOPERATIVE_TASK_CANCELLED.load(Ordering::Relaxed),
        "the managed task should have been cancelled (and joined) during shutdown"
    );
}

// -- Setup failure: tasks spawned before the error are still cancelled. --

static SETUP_FAILURE_TASK_STARTED: AtomicBool = AtomicBool::new(false);
static SETUP_FAILURE_TASK_CANCELLED: AtomicBool = AtomicBool::new(false);

#[phoxal::service(id = "managed-setup-failure", config = (), api = ())]
struct ManagedSetupFailure;

#[phoxal::behavior]
impl ManagedSetupFailure {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed("setup-failure-loop", async move {
            SETUP_FAILURE_TASK_STARTED.store(true, Ordering::Relaxed);
            let _guard = SetFlagOnDrop(&SETUP_FAILURE_TASK_CANCELLED);
            std::future::pending::<()>().await;
        });

        while !SETUP_FAILURE_TASK_STARTED.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }

        Err(std::io::Error::other("setup deliberately fails after spawning a managed task").into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_failure_cancels_spawned_managed_tasks() {
    let launch = local_launch("setup-failure", "managed-setup-failure-1");
    let shutdown = std::future::pending();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_with::<ManagedSetupFailure, _, _>(launch, RealClock::new(), shutdown),
    )
    .await
    .expect("runner must not hang cleaning up a managed task after setup fails");

    assert!(SETUP_FAILURE_TASK_STARTED.load(Ordering::Relaxed));
    let err = result.expect_err("setup failure should still be returned");
    assert!(
        err.to_string().contains("setup deliberately fails"),
        "setup error should be preserved: {err}"
    );
    assert!(
        SETUP_FAILURE_TASK_CANCELLED.load(Ordering::Relaxed),
        "managed task spawned before setup failed should be cancelled and joined"
    );
}

// -- Join timeout: a managed task that ignores cancellation is reported unjoined,
//    but the runner still proceeds (does not hang or fault). --

static STUCK_TASK_STARTED: AtomicBool = AtomicBool::new(false);
static STUCK_TASK_FINISHED: AtomicBool = AtomicBool::new(false);

#[phoxal::service(id = "managed-stuck-shutdown", config = (), api = ())]
struct ManagedStuckShutdown;

#[phoxal::behavior]
impl ManagedStuckShutdown {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        ctx.spawn_managed("uncancellable-task", async move {
            STUCK_TASK_STARTED.store(true, Ordering::Relaxed);
            // Synchronous blocking work cannot observe Tokio task cancellation
            // until it returns to the async scheduler. Keep it finite so the
            // test runtime can shut down promptly after proving the runner did
            // not wait for it past the grace deadline.
            std::thread::sleep(Duration::from_millis(1500));
            STUCK_TASK_FINISHED.store(true, Ordering::Relaxed);
        });

        while !STUCK_TASK_STARTED.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }

        Ok((Self, ()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_reports_unjoined_managed_tasks_after_grace_elapses() {
    let mut launch = local_launch("stuck-shutdown", "managed-stuck-shutdown-1");
    launch.shutdown_grace_ms = 150;
    let shutdown = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The task is inside a synchronous section when cancellation is requested,
    // so the runner cannot join it cooperatively. The runner must still return
    // within a bounded time and leave the task reported as unjoined rather than
    // waiting for the synchronous section to finish.
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run_with::<ManagedStuckShutdown, _, _>(launch, RealClock::new(), shutdown),
    )
    .await
    .expect("runner must not hang past its own timeout budget waiting on a stuck task");
    let elapsed = started.elapsed();

    assert!(STUCK_TASK_STARTED.load(Ordering::Relaxed));
    result.expect("an unjoined-at-grace managed task must not itself fault the runtime");
    assert!(
        elapsed < Duration::from_millis(700),
        "shutdown should be bounded by the grace budgets, took {elapsed:?}"
    );
    assert!(
        !STUCK_TASK_FINISHED.load(Ordering::Relaxed),
        "runner should return before a cancellation-ignoring task finishes"
    );
}
