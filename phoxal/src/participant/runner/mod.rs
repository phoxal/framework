//! The runner: owns the bus connection, clock, step scheduling, serialized
//! query dispatch, and graceful shutdown.
//!
//! `phoxal::run::<R>()` builds a blocking Tokio runtime and runs the participant to
//! completion; `phoxal::tokio::run::<R>().await` is the async entrypoint for
//! custom Tokio mains.
//!
//! Setup returns separate `State` and `Api` values. One main-loop task owns
//! mutable `State`; due steps, timeline resets, and typed queries all take
//! turns on that task. There is no snapshot projection or concurrent handler
//! branch.

use std::future::Future;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::api;
use crate::bus::{LocalInstant, RobotInstant, StepToken, Subscriber, TimelineId};
use crate::participant::api::{Participant, ParticipantConfig};
use crate::participant::bus_log::{self, BusLogTask};
use crate::participant::clock::real::RealClock;
use crate::participant::clock::simulation::SimulationClock;
use crate::participant::clock::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::participant::context::{ResetContext, SetupContext, StepContext, TimelineRetention};
use crate::participant::launch::SupervisedLaunch;
#[cfg(feature = "test-harness")]
use crate::participant::launch::TestHarness;
use crate::participant::managed::{ManagedTaskExit, ManagedTaskPolicy, ManagedTasks};
use crate::participant::runtime_performance::{RuntimePerformance, RuntimePerformancePublisher};
use crate::participant::scheduler::simulation::{SimulationClockAdvance, SimulationClockHandle};
use crate::participant::scheduler::{AnyStepScheduler, SchedulerTick, StepSchedule, StepScheduler};
use phoxal_bundle::Sha256Digest;
use phoxal_bus::{BusConfig, BusHandle, BusOwner, ParticipantReadyToken};
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::launch::ClockMode;

pub(crate) mod inputs;
pub(crate) mod query;
pub(crate) mod signal;
pub(crate) mod teardown;

use inputs::{participant_config, participant_inputs_for_launch};
use phoxal_bundle::ParticipantRuntimeInputs;
use query::QuerySurface;
use signal::shutdown_signal;
use teardown::{
    ShutdownDeadline, Teardown, TeardownReport, abandon_setup, abandon_startup, combine,
};

/// How often the runner wakes for work that is not a step: publishing the
/// runtime-performance rollup, and re-checking clock discipline.
const RUNTIME_PERFORMANCE_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// A sticky lifecycle stop request. The source future is polled during every
/// asynchronous startup boundary and the request remains observable after the
/// source has completed, so a signal cannot be lost between setup and Ready.
struct ShutdownRequest {
    requested: AtomicBool,
    notify: tokio::sync::Notify,
}

impl ShutdownRequest {
    fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    fn trigger(&self) {
        if !self.requested.swap(true, Ordering::Release) {
            self.notify.notify_waiters();
        }
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        if self.is_requested() {
            return;
        }
        let notified = self.notify.notified();
        if self.is_requested() {
            return;
        }
        notified.await;
    }
}

/// Couples an injected shutdown source (or the Unix signal future) to a sticky
/// request that startup and the main loop can race independently.
struct ShutdownController<S> {
    request: Arc<ShutdownRequest>,
    source: Pin<Box<S>>,
}

impl<S> ShutdownController<S>
where
    S: Future<Output = ()>,
{
    fn new(source: S) -> Self {
        Self {
            request: Arc::new(ShutdownRequest::new()),
            source: Box::pin(source),
        }
    }

    fn is_requested(&self) -> bool {
        self.request.is_requested()
    }

    async fn wait(&mut self) {
        if self.request.is_requested() {
            return;
        }
        tokio::select! {
            biased;
            _ = self.request.wait() => {},
            _ = &mut self.source => self.request.trigger(),
        }
    }
}

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// The compiled runtime record decides whether clock selection exists at all;
/// the process launch carries no scheduler override. The default binary entrypoint is
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
pub fn run<R: Participant>() -> crate::Result<()> {
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tokio_runtime.block_on(run_async::<R>())
}

/// Async host runner for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
pub async fn run_async<R: Participant>() -> crate::Result<()> {
    // Every real entry path (`run`, and this function directly as
    // `phoxal::tokio::run`) passes through here first, which is what makes
    // this the one place that needs to touch `R`'s embedded metadata static
    // to keep the ELF linker from garbage-collecting it - see
    // `Participant::__retain_embedded_metadata`'s docs.
    R::__retain_embedded_metadata();

    bus_log::init_tracing();
    // Parse the supervised process contract before opening anything. A
    // supervisor's signal is installed immediately after parsing so startup
    // teardown still follows the same steady-state path.
    let launch = SupervisedLaunch::parse()?;
    let shutdown = shutdown_signal()?;

    run_with::<R, _>(launch, shutdown).await
}

/// Run a participant against a strict launch and shutdown trigger, on a bus
/// this function opens and closes. The runner owns the host clock; simulators
/// therefore have no clock parameter to receive or override.
async fn run_with<R, S>(launch: SupervisedLaunch, shutdown: S) -> crate::Result<()>
where
    R: Participant,
    S: Future<Output = ()>,
{
    let mut shutdown = ShutdownController::new(shutdown);
    // Convert the Clap millisecond field once at the process boundary. The
    // remainder of the lifecycle carries a typed duration.
    let shutdown_grace = Duration::from_millis(launch.shutdown_grace_ms);

    // Validate and select the persisted participant before opening the bus.
    // A malformed runtime document or unknown topology id is a local startup
    // failure, never a bus-visible participant.
    // Bundle validation and exact participant selection happen before any bus
    // session exists. A malformed bundle therefore has no producer or wire
    // side effects to clean up.
    let bundle = participant_inputs_for_launch(&launch.bundle_root, &launch.participant_id)?;
    if launch.participant_id.as_str() != R::ID {
        anyhow::bail!(
            "supervised participant id '{}' does not match this binary's compiled id '{}'",
            launch.participant_id,
            R::ID
        );
    }
    if bundle.participant.kind != R::KIND {
        anyhow::bail!(
            "runtime participant '{}' has kind {:?}, but this binary declares {:?}",
            launch.participant_id,
            bundle.participant.kind,
            R::KIND
        );
    }
    verify_current_executable(bundle.participant.binary.digest)?;
    let process_config_schema: serde_json::Value = serde_json::from_str(R::Config::SCHEMA_JSON)
        .map_err(|error| {
            anyhow::anyhow!(
                "binary '{}' carries an invalid compiled config schema: {error}",
                R::ID
            )
        })?;
    if process_config_schema != bundle.participant.binary.compatibility.config_schema {
        anyhow::bail!(
            "runtime config schema for participant '{}' does not match this binary",
            launch.participant_id
        );
    }
    // Deserialize the selected config while the process is still local. A
    // custom `Deserialize` implementation may reject a value that its JSON
    // Schema accepts; that must not become a transport-visible startup error.
    let config = participant_config::<R::Config>(bundle.participant.config.as_ref())?;
    let origin = launch
        .execution_origin
        .ok_or(TimeUnsynchronized::MissingOrigin)?;
    let clock = RealClock::new(origin)?;
    let clock_mode = bundle.participant.clock;
    validate_clock_inputs::<R, _>(clock_mode, &clock)?;

    // One line, not a per-attempt one: a participant racing a router that has
    // not opened its listener yet can take several seconds to connect. Without
    // this, that gap looks like a silent hang rather than expected startup.
    tracing::info!(
        target: "phoxal.runtime",
        endpoints = ?launch.connect_endpoints,
        "connecting to the bus"
    );
    let (owner, bus) = tokio::select! {
        biased;
        _ = shutdown.wait() => return Ok(()),
        result = BusOwner::open(BusConfig::for_participant(
            launch.execution_id,
            launch.participant_id.clone(),
            launch.connect_endpoints.clone(),
        )) => result?,
    };

    run_inner::<R, RealClock, S>(
        &bus,
        Some(owner),
        &launch.participant_id,
        shutdown_grace,
        Some(bundle),
        config,
        clock_mode,
        clock,
        None,
        &mut shutdown,
    )
    .await
}

/// Hash the executable image that is actually running this participant.
///
/// Linux's `/proc/self/exe` is opened directly: if the bundle path is replaced
/// or unlinked after `execve`, the kernel still exposes the original executable
/// inode through that handle. Targets without an equivalent secure primitive
/// fail closed before transport startup; `current_exe()` is deliberately not
/// used because its path lookup is vulnerable to replacement between lookup
/// and open.
#[cfg(target_os = "linux")]
fn verify_current_executable(expected: Sha256Digest) -> crate::Result<()> {
    let (path, executable) = open_current_executable()?;
    let actual = Sha256Digest::from_reader(executable).map_err(|error| {
        anyhow::anyhow!(
            "failed to read running executable {}: {error}",
            path.display()
        )
    })?;
    if actual != expected {
        anyhow::bail!(
            "running executable {} does not match the runtime bundle binary digest (expected {}, got {})",
            path.display(),
            expected,
            actual
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_current_executable(_expected: Sha256Digest) -> crate::Result<()> {
    Err(UnsupportedSecureExecutableIdentification.into())
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, thiserror::Error)]
#[error(
    "secure executable identification is unsupported on this target; supervised launch is disabled"
)]
struct UnsupportedSecureExecutableIdentification;

#[cfg(target_os = "linux")]
fn open_current_executable() -> crate::Result<(PathBuf, std::fs::File)> {
    let proc_path = Path::new("/proc/self/exe");
    let display_path = std::fs::read_link(proc_path).unwrap_or_else(|_| proc_path.into());
    let executable = std::fs::File::open(proc_path).map_err(|error| {
        anyhow::anyhow!(
            "failed to open the running executable {}: {error}",
            display_path.display()
        )
    })?;
    Ok((display_path, executable))
}

/// Validate scheduler selection and the initial clock discipline before any
/// supervised transport is opened. The lifecycle repeats the construction to
/// retain its scheduler handle, but this pass makes malformed clock inputs a
/// local startup failure.
fn validate_clock_inputs<R, C>(clock_mode: ClockMode, clock: &C) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
{
    let reading = clock.read();
    if clock_mode == ClockMode::Real
        && let ClockReading::Unsynchronized(reason) = reading
    {
        return Err(ClockDisciplineLost { reason }.into());
    }
    AnyStepScheduler::for_clock_mode(clock_mode, R::__step_schedule(), reading.instant())?;
    Ok(())
}

/// Run a participant on a **caller-owned** bus using explicit test-harness
/// input. Unlike the supervised entrypoint, this does not open or close the
/// bus - the caller controls its lifecycle.
///
/// This is the embedding seam for co-locating participants on a single in-process
/// [`Bus`] in an explicit test harness. Bus metadata `source` identity is a
/// property of the bus, so participants sharing one bus publish under its
/// participant attribution.
#[cfg(feature = "test-harness")]
pub async fn run_test_harness<R, S>(
    bus: &BusHandle,
    harness: TestHarness,
    shutdown: S,
) -> crate::Result<()>
where
    R: Participant,
    S: Future<Output = ()>,
{
    bus_log::init_tracing();
    let query_reply_delay = harness.query_reply_delay;
    let clock = RealClock::new(harness.execution_origin)?;
    let config = participant_config::<R::Config>(None)?;
    validate_clock_inputs::<R, _>(ClockMode::Real, &clock)?;
    let mut shutdown = ShutdownController::new(shutdown);
    run_inner::<R, RealClock, S>(
        bus,
        None,
        &harness.participant_id,
        Duration::from_millis(harness.shutdown_grace_ms),
        None,
        config,
        ClockMode::Real,
        clock,
        query_reply_delay,
        &mut shutdown,
    )
    .await
}

/// Deterministic clock-injection seam for checked participants.
///
/// Behind the `test-harness` feature: a shipped participant binary that could
/// substitute a clock it drives itself could stamp instants it never reached.
#[cfg(feature = "test-harness")]
#[doc(hidden)]
pub async fn run_test_harness_with_clock<R, C, S>(
    bus: &BusHandle,
    harness: TestHarness,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: Participant
        + crate::__private::surface::TypedIoSurface
        + crate::__private::surface::SchedulableSurface,
    C: ClockSource,
    S: Future<Output = ()>,
{
    bus_log::init_tracing();
    let query_reply_delay = harness.query_reply_delay;
    let config = participant_config::<R::Config>(None)?;
    validate_clock_inputs::<R, _>(ClockMode::Real, &clock)?;
    let mut shutdown = ShutdownController::new(shutdown);
    run_inner::<R, C, S>(
        bus,
        None,
        &harness.participant_id,
        Duration::from_millis(harness.shutdown_grace_ms),
        None,
        config,
        ClockMode::Real,
        clock,
        query_reply_delay,
        &mut shutdown,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "the runner keeps ownership, selected inputs, clock mode, and shutdown explicit"
)]
async fn run_inner<R, C, S>(
    bus: &BusHandle,
    owner: Option<BusOwner>,
    participant_id: &ParticipantId,
    shutdown_grace: Duration,
    bundle: Option<ParticipantRuntimeInputs>,
    config: R::Config,
    clock_mode: ClockMode,
    clock: C,
    query_reply_delay: Option<Duration>,
    shutdown: &mut ShutdownController<S>,
) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let (bus_logs, bus_log_task) = bus_log::attach(bus.clone(), participant_id.as_str());
    let result = run_lifecycle::<R, C, S>(
        bus,
        owner,
        participant_id,
        shutdown_grace,
        bundle,
        config,
        clock_mode,
        clock,
        query_reply_delay,
        shutdown,
        bus_log_task,
    )
    .await;
    bus_logs.shutdown();
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "startup keeps the strict launch selection, validated bundle, scheduler, clock, and owned tasks explicit"
)]
async fn run_lifecycle<R, C, S>(
    bus: &BusHandle,
    mut owner: Option<BusOwner>,
    participant_id: &ParticipantId,
    shutdown_grace: Duration,
    bundle: Option<ParticipantRuntimeInputs>,
    config: R::Config,
    clock_mode: ClockMode,
    clock: C,
    query_reply_delay: Option<Duration>,
    shutdown: &mut ShutdownController<S>,
    bus_log_task: BusLogTask,
) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let schedule = R::__step_schedule();
    // A validated runtime record is the sole authority for scheduler policy.
    // The only caller without a bundle is the explicit in-process test
    // harness, which uses the participant's ordinary real-clock default.
    let reading = clock.read();
    if clock_mode == ClockMode::Real
        && let ClockReading::Unsynchronized(reason) = reading
    {
        // Only a real participant needs this: a clockless one was never given
        // an origin, and a simulation one has no world history until the
        // authority publishes its first step.
        // Starting a real participant on an untrustworthy clock would mean
        // every deadline it owns is measured against a number it cannot
        // defend. This is ordinary failure, so the supervisor's restart and
        // start-limit policy decides what happens next.
        return close_owner_with_result(
            Err(ClockDisciplineLost { reason }.into()),
            owner,
            ShutdownDeadline::from_now(shutdown_grace),
        )
        .await;
    }
    let (scheduler, clock_handle) =
        match AnyStepScheduler::for_clock_mode(clock_mode, schedule, reading.instant()) {
            Ok(value) => value,
            Err(error) => {
                return close_owner_with_result(
                    Err(error),
                    owner,
                    ShutdownDeadline::from_now(shutdown_grace),
                )
                .await;
            }
        };
    let effective_clock = match &scheduler {
        AnyStepScheduler::Simulation(simulation) => {
            RunnerClock::Simulation(simulation.simulation_clock())
        }
        AnyStepScheduler::Real(_) | AnyStepScheduler::Disabled => RunnerClock::Delegated(clock),
    };
    // Subscribe before setup so the simulation clock can advance while setup runs.
    match Runner::<R, C>::start(
        bus,
        &mut owner,
        participant_id,
        shutdown_grace,
        shutdown,
        bundle,
        config,
        effective_clock,
        scheduler,
        schedule,
        clock_mode,
        RunnerTasks {
            simulation_clock: clock_handle,
            bus_log: bus_log_task,
            query_reply_delay,
        },
    )
    .await
    {
        StartOutcome::Ready(runner) => runner.run(shutdown).await,
        StartOutcome::Terminal { result, deadline } => {
            close_owner_with_result(result, owner, deadline).await
        }
    }
}

async fn close_owner_with_result<T>(
    primary: crate::Result<T>,
    owner: Option<BusOwner>,
    deadline: ShutdownDeadline,
) -> crate::Result<T> {
    let Some(owner) = owner else {
        return primary;
    };
    match owner.close_until(deadline.instant()).await {
        Ok(close) if close.is_clean() => primary,
        Ok(close) => teardown::combine(
            primary,
            teardown::TeardownReport {
                bus_close_report: Some(close),
                ..teardown::TeardownReport::default()
            },
        ),
        Err(error) => teardown::combine(
            primary,
            teardown::TeardownReport {
                bus_close_error: Some(error.into()),
                ..teardown::TeardownReport::default()
            },
        ),
    }
}

/// The failure a participant reports when it cannot trust its own clock.
///
/// Typed rather than flattened into a message: the reason stays inspectable for
/// anything that has to tell the triggers apart, and it still renders into the
/// text the supervisor keeps as failure evidence - the operator's only handle on
/// which trigger fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("clock discipline lost: {reason}")]
pub(crate) struct ClockDisciplineLost {
    pub(crate) reason: TimeUnsynchronized,
}

/// Why the main loop stopped.
///
/// Every variant but [`LoopExit::ShutdownRequested`] means the same thing to the
/// caller: run the ordinary teardown - which parks the hardware through
/// `Participant::shutdown` - and then report a failure, so the supervisor's
/// restart and start-limit policy decides what happens next. None of them is a
/// distinct "failure mode" the participant handles itself.
enum LoopExit {
    /// The host asked the participant to stop. The only exit that is not a
    /// failure.
    ShutdownRequested,
    /// A runner-owned task violated its completion policy.
    ManagedTaskFaulted(ManagedTaskExit),
    /// The clock stopped being trustworthy, so no further step could be timed.
    ClockDisciplineLost(TimeUnsynchronized),
    /// `Participant::reset` refused the replacement world history.
    ResetFailed(anyhow::Error),
    /// A scheduled state transition failed. Step failures are terminal so the
    /// runner can park the participant immediately instead of continuing with
    /// state whose invariants the transition may have left unknown.
    StepFailed(anyhow::Error),
    /// The bounded runner-owned query reply queue could not accept a response
    /// without making the serialized owner await transport back-pressure.
    QueryDispatchFailed(anyhow::Error),
}

impl LoopExit {
    fn into_result(self) -> crate::Result<()> {
        match self {
            LoopExit::ShutdownRequested => Ok(()),
            LoopExit::ManagedTaskFaulted(exit) => Err(exit.into()),
            LoopExit::ClockDisciplineLost(reason) => Err(ClockDisciplineLost { reason }.into()),
            LoopExit::ResetFailed(error) => Err(error),
            LoopExit::StepFailed(error) => Err(error),
            LoopExit::QueryDispatchFailed(error) => Err(error),
        }
    }
}

/// The runner's effective timestamp clock, chosen once the scheduler is built.
///
/// In real mode it delegates to the runner-owned host [`RealClock`] (or, behind
/// the `test-harness` feature, to a clock the caller drives). In simulation mode
/// it is the [`SimulationClock`] that shares the simulation scheduler's live
/// `simulation/clock` feed, so stamped time and step-release time stay in the
/// one simulation domain (see the `SimulationClock` docs for why wall-stamping a
/// simulated participant is wrong).
enum RunnerClock<C: ClockSource> {
    Delegated(C),
    Simulation(SimulationClock),
}

impl<C: ClockSource> ClockSource for RunnerClock<C> {
    fn read(&self) -> ClockReading {
        match self {
            RunnerClock::Delegated(clock) => clock.read(),
            RunnerClock::Simulation(clock) => clock.read(),
        }
    }
}

/// One participant, running.
///
/// The runner owns everything the loop reads or mutates - the participant and
/// its `Api`/`State`, the bus session, the effective clock and scheduler, the
/// query surface, the managed tasks, and the runtime-performance accounting - so
/// that "drive this participant" is one receiver rather than a parameter list
/// that has to be threaded through every step of the lifecycle.
struct Runner<R: Participant, C: ClockSource> {
    participant: R,
    api: R::Api,
    state: R::State,
    bus: BusHandle,
    /// The unique session owner for supervised runs. Caller-owned embedding
    /// uses `None` and therefore cannot assert participant Ready.
    owner: Option<BusOwner>,
    clock: RunnerClock<C>,
    scheduler: AnyStepScheduler,
    schedule: Option<StepSchedule>,
    clock_mode: ClockMode,
    timeline_retentions: Vec<TimelineRetention>,
    queries: Option<QuerySurface<R>>,
    runtime_performance_publisher: RuntimePerformancePublisher,
    runtime_performance: RuntimePerformance,
    managed_tasks: ManagedTasks,
    /// The participant's Ready lease. It is revoked before any
    /// shutdown work starts, so observers never see Ready while resources are
    /// being unwound.
    ready: Option<ParticipantReadyToken>,
    shutdown_grace: Duration,
}

/// Framework tasks that must be registered before setup can declare Ready.
/// Keeping them together makes the startup boundary explicit and prevents the
/// runner's setup signature from turning into an unowned parameter list.
struct RunnerTasks {
    simulation_clock: Option<SimulationClockHandle>,
    bus_log: BusLogTask,
    query_reply_delay: Option<Duration>,
}

enum StartOutcome<T> {
    Ready(T),
    Terminal {
        result: crate::Result<()>,
        deadline: ShutdownDeadline,
    },
}

fn startup_terminal<T>(
    primary: crate::Result<()>,
    report: TeardownReport,
    deadline: ShutdownDeadline,
) -> StartOutcome<T> {
    StartOutcome::Terminal {
        result: combine(primary, report),
        deadline,
    }
}

async fn startup_teardown<T, R>(
    managed_tasks: ManagedTasks,
    participant: &R,
    api: &R::Api,
    state: &mut R::State,
    shutdown_grace: Duration,
    primary: crate::Result<()>,
) -> StartOutcome<T>
where
    R: Participant,
{
    let deadline = ShutdownDeadline::from_now(shutdown_grace);
    let report = Teardown {
        managed_tasks,
        deadline,
    }
    .run(participant, api, state)
    .await;
    startup_terminal(primary, report, deadline)
}

impl<R: Participant, C: ClockSource> Runner<R, C> {
    /// Resolve the selected runtime record, run `Participant::setup`, and declare everything the
    /// participant announced - after which the participant is live on the graph.
    ///
    /// Every failure after `setup` succeeds still runs the full teardown, so a
    /// server or liveliness declaration that fails cannot bypass the
    /// participant's hardware-safety hook.
    #[expect(
        clippy::too_many_arguments,
        reason = "startup keeps the validated bundle, scheduler, clock, and framework tasks explicit"
    )]
    async fn start<S>(
        bus: &BusHandle,
        owner: &mut Option<BusOwner>,
        participant_id: &ParticipantId,
        shutdown_grace: Duration,
        shutdown: &mut ShutdownController<S>,
        bundle: Option<ParticipantRuntimeInputs>,
        config: R::Config,
        clock: RunnerClock<C>,
        scheduler: AnyStepScheduler,
        schedule: Option<StepSchedule>,
        clock_mode: ClockMode,
        tasks: RunnerTasks,
    ) -> StartOutcome<Self>
    where
        S: Future<Output = ()>,
    {
        // The selected runtime record (or explicit test harness) was already
        // deserialized before entering this transport-owned startup path.
        let mut ctx = SetupContext::<R>::new(bus.clone(), bundle);
        ctx.spawn_managed_with(
            "bus-log-drain",
            ManagedTaskPolicy::Finite,
            tasks.bus_log.run(),
        );
        if let Some(handle) = tasks.simulation_clock {
            ctx.spawn_managed(
                "simulation-clock-ingest",
                simulation_clock_feed(bus.clone(), handle),
            );
        }
        let participant = R::__new();
        let setup = participant.setup(&mut ctx, config);
        let (mut state, api) = match tokio::select! {
            biased;
            _ = shutdown.wait() => {
                let deadline = ShutdownDeadline::from_now(shutdown_grace);
                let report = abandon_startup(ctx.take_managed_tasks(), deadline).await;
                return startup_terminal(Ok(()), report, deadline);
            }
            result = setup => result,
        } {
            Ok(pair) => pair,
            Err(error) => {
                let deadline = ShutdownDeadline::from_now(shutdown_grace);
                let error = abandon_setup(ctx.take_managed_tasks(), error, deadline).await;
                return StartOutcome::Terminal {
                    result: Err(error),
                    deadline,
                };
            }
        };
        // From here on the runner - not `SetupContext` - owns watching the tasks
        // `ctx.spawn_managed(...)` started for an unexpected exit, and
        // cancelling/joining them at shutdown.
        let mut managed_tasks = ctx.take_managed_tasks();
        let timeline_retentions = ctx.take_timeline_retentions();
        let query_registrations = ctx.take_query_registrations();
        let query_reply_delay = tasks.query_reply_delay;

        let mut queries = match tokio::select! {
            biased;
            _ = shutdown.wait() => {
                return startup_teardown(
                    managed_tasks,
                    &participant,
                    &api,
                    &mut state,
                    shutdown_grace,
                    Ok(()),
                ).await;
            }
            result = QuerySurface::declare(
                bus,
                query_registrations,
                &mut managed_tasks,
                query_reply_delay,
            ) => result,
        } {
            Ok(queries) => queries,
            Err(error) => {
                return startup_teardown(
                    managed_tasks,
                    &participant,
                    &api,
                    &mut state,
                    shutdown_grace,
                    Err(error),
                )
                .await;
            }
        };

        // Setup and query declaration may have started tasks whose failure is
        // already ready to observe. Drain those completions before acquiring
        // the Ready/liveliness token so a failed critical task can never pass
        // through a transient "ready" state.
        if let Some(exit) = managed_tasks.try_next_unexpected_exit() {
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Err(exit.into()),
            )
            .await;
        }
        if shutdown.is_requested() {
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Ok(()),
            )
            .await;
        }
        // Ready acquisition is itself a lifecycle boundary. Race the bus
        // declaration against already-supervised task completion so a
        // Critical setup/query failure cannot win the await and briefly make
        // an unhealthy participant visible.
        let ready = match owner.as_ref() {
            None => {
                tokio::select! {
                    biased;
                    _ = shutdown.wait() => {
                        if let Some(queries) = queries.take() {
                            queries.close();
                        }
                        return startup_teardown(
                            managed_tasks,
                            &participant,
                            &api,
                            &mut state,
                            shutdown_grace,
                            Ok(()),
                        ).await;
                    }
                    _ = tokio::task::yield_now() => {}
                }
                None
            }
            Some(owner) => Some(tokio::select! {
                biased;
                _ = shutdown.wait() => {
                    if let Some(queries) = queries.take() {
                        queries.close();
                    }
                    return startup_teardown(
                        managed_tasks,
                        &participant,
                        &api,
                        &mut state,
                        shutdown_grace,
                        Ok(()),
                    ).await;
                }
                exit = managed_tasks.next_unexpected_exit() => {
                    if let Some(queries) = queries.take() {
                        queries.close();
                    }
                    return startup_teardown(
                        managed_tasks,
                        &participant,
                        &api,
                        &mut state,
                        shutdown_grace,
                        Err(exit.into()),
                    ).await;
                }
                result = owner.declare_participant_ready() => match result {
                    Ok(token) => token,
                    Err(error) => {
                        if let Some(queries) = queries.take() {
                            queries.close();
                        }
                        return startup_teardown(
                            managed_tasks,
                            &participant,
                            &api,
                            &mut state,
                            shutdown_grace,
                            Err(error.into()),
                        ).await;
                    }
                },
            }),
        };

        // Do not accept the token merely because its await won the race. Give
        // task completions that became ready during declaration a scheduling
        // turn, drain them, and revoke the just-acquired token before any
        // Ready announcement or Runner is returned.
        tokio::task::yield_now().await;
        if shutdown.is_requested() {
            drop(ready);
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Ok(()),
            )
            .await;
        }
        if let Some(exit) = managed_tasks.try_next_unexpected_exit() {
            drop(ready);
            if let Some(queries) = queries.take() {
                queries.close();
            }
            return startup_teardown(
                managed_tasks,
                &participant,
                &api,
                &mut state,
                shutdown_grace,
                Err(exit.into()),
            )
            .await;
        }

        tracing::info!(
            target: "phoxal.runtime",
            id = R::ID,
            participant = %participant_id,
            "runtime ready"
        );
        StartOutcome::Ready(Runner {
            participant,
            api,
            state,
            bus: bus.clone(),
            owner: owner.take(),
            clock,
            scheduler,
            schedule,
            clock_mode,
            timeline_retentions,
            queries,
            // Portable runtime evidence is measured at the runner-owned
            // step/buffer boundaries. No OS sampler or participant-authored
            // telemetry is involved.
            runtime_performance_publisher: RuntimePerformancePublisher::attach(bus.clone()),
            runtime_performance: RuntimePerformance::new(schedule),
            managed_tasks,
            ready,
            shutdown_grace,
        })
    }

    /// Drive the participant until it stops, then wind it down. The teardown
    /// runs before the exit is turned into a result, so the hardware is parked
    /// whether the loop ended by request or by fault.
    async fn run<S>(mut self, shutdown: &mut ShutdownController<S>) -> crate::Result<()>
    where
        S: Future<Output = ()>,
    {
        let exit = self.main_loop(shutdown).await;
        let primary = exit.into_result();
        let report = self.finish().await;
        combine(primary, report)
    }

    async fn finish(self) -> TeardownReport {
        let Runner {
            participant,
            api,
            mut state,
            queries,
            managed_tasks,
            owner,
            ready,
            shutdown_grace,
            ..
        } = self;

        // Ready is revoked first: teardown must never leave a live lease while
        // participant resources are being unwound.
        drop(ready);

        // Query receive tasks stop next: nothing after this point serves a
        // request, and one arriving mid-teardown must not reach state the
        // shutdown hook is already unwinding.
        if let Some(queries) = queries {
            queries.close();
        }
        let deadline = ShutdownDeadline::from_now(shutdown_grace);
        let mut report = Teardown {
            managed_tasks,
            deadline,
        }
        .run(&participant, &api, &mut state)
        .await;
        if let Some(owner) = owner {
            match owner.close_until(deadline.instant()).await {
                Ok(close) if close.is_clean() => {}
                Ok(close) => report.bus_close_report = Some(close),
                Err(error) => report.bus_close_error = Some(error.into()),
            }
        }
        report
    }

    async fn main_loop<S>(&mut self, shutdown: &mut ShutdownController<S>) -> LoopExit
    where
        S: Future<Output = ()>,
    {
        let period = self.schedule.map(|schedule| schedule.period());
        let mut step_index: u64 = 0;
        let mut active_timeline: Option<TimelineId> = None;
        let mut simulation_time_rx = self.scheduler.simulation_time_receiver();
        // The simulation clock feed starts before `Participant::setup`. If setup takes long
        // enough for the authority's first world step to arrive, a newly-cloned
        // watch receiver sees that value as its initial state and has no change
        // notification to deliver. Establish that already-current world history
        // without invoking reset: there was no prior participant execution, but its
        // ingress barrier and first cadence still matter.
        let initial_time = self.scheduler.now();
        if let Some(initial_time) = initial_time.filter(|_| simulation_time_rx.is_some()) {
            active_timeline = Some(initial_time.timeline());
            retain_timeline(&self.timeline_retentions, initial_time.timeline());
        }
        let mut last_step_at = initial_time;
        // The next tick's *robot* due time - what the runner asks the scheduler
        // to release at, separate from the host-monotonic beat below.
        let mut next_step_target =
            initial_time.and_then(|at| period.map(|period| advance_step_deadline(at, period, 0)));
        let mut beat = tokio::time::interval_at(
            tokio::time::Instant::now(),
            RUNTIME_PERFORMANCE_TICK_INTERVAL,
        );
        beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Order matters: shutdown first, then a managed-task fault (both are
                // "stop the loop" events and should preempt routine work), then the
                // runtime-performance publication tick, then a *due* step, then
                // server queries. Publication is cheap and must not be starved by
                // an overloaded participant; due steps still take priority over a
                // steady query backlog.
                biased;
                _ = shutdown.wait() => return LoopExit::ShutdownRequested,
                exit = self.managed_tasks.next_unexpected_exit() => {
                    tracing::error!(
                        target: "phoxal.runtime",
                        task = %exit.name,
                        panic = exit.panic_message.as_deref(),
                        "managed task exited unexpectedly; faulting the participant"
                    );
                    return LoopExit::ManagedTaskFaulted(exit);
                }
                fired_at = simulation_time_change(&mut simulation_time_rx) => {
                    if active_timeline == Some(fired_at.timeline()) {
                        continue;
                    }

                    // Timelines are opaque identities, not ordered generations. Any
                    // different one establishes a replacement world history. This
                    // branch is independent of `Participant::step`, so clocked server-only
                    // services receive the same serialized reset lifecycle.
                    let previous_timeline = active_timeline.replace(fired_at.timeline());
                    retain_timeline(&self.timeline_retentions, fired_at.timeline());
                    if let Some(previous_timeline) = previous_timeline {
                        let reset = ResetContext {
                            previous_timeline,
                            new_timeline: fired_at.timeline(),
                        };
                        if let Err(error) = self
                            .participant
                            .reset(reset, &self.api, &mut self.state)
                        {
                            return LoopExit::ResetFailed(error);
                        }
                    }
                    next_step_target =
                        period.map(|period| advance_step_deadline(fired_at, period, 0));
                    step_index = 0;
                    last_step_at = Some(fired_at);
                    self.runtime_performance.reset(self.schedule);
                }
                _ = beat.tick() => {
                    // A real participant with no `Participant::step` schedule would otherwise
                    // check its clock once at startup and never again, and go on
                    // serving queries from state it cannot date. This beat
                    // is its only recurring one, so clock discipline is checked
                    // here too - a stepping participant reaches the same check
                    // sooner, in its own step arm.
                    //
                    // Simulation is excluded on purpose: there, "unsynchronized"
                    // means the world authority has not published a first step yet,
                    // which is a world that has not started rather than a clock
                    // that was lost.
                    let faulted = LocalInstant::clock_faulted()
                        .then_some(TimeUnsynchronized::ClockFault)
                        .or_else(|| match (period, self.clock_mode) {
                            (None, ClockMode::Real) => match self.clock.read() {
                                ClockReading::Unsynchronized(reason) => Some(reason),
                                ClockReading::Synchronized(_) => None,
                            },
                            _ => None,
                        });
                    if let Some(reason) = faulted {
                        tracing::error!(
                            target: "phoxal.runtime",
                            error = %reason,
                            "clock discipline lost; failing the participant"
                        );
                        return LoopExit::ClockDisciplineLost(reason);
                    }
                    if let Some(rollup) = self.runtime_performance.take_rollup(&self.bus) {
                        self.runtime_performance_publisher.publish(rollup);
                    }
                }
                SchedulerTick { fired_at, missed_ticks }
                    = self.scheduler.wait_until_due(next_step_target) =>
                {
                    let (Some(period), Some(target)) = (period, next_step_target) else { continue };

                    // A boot-clock read failed somewhere in this process - the bus
                    // stamper, a driver's permit, an arbiter's silence deadline.
                    // Each of those failed closed on its own, but a process that
                    // cannot read its own clock does not get to carry on once reads
                    // start working again: recovery is a fresh process.
                    if LocalInstant::clock_faulted() {
                        tracing::error!(
                            target: "phoxal.runtime",
                            error = %TimeUnsynchronized::ClockFault,
                            "clock discipline lost; failing the participant"
                        );
                        return LoopExit::ClockDisciplineLost(TimeUnsynchronized::ClockFault);
                    }

                    if fired_at.timeline() != target.timeline() {
                        // The independent simulation-time branch above owns
                        // timeline replacement. A simultaneously-ready watch
                        // notification is biased ahead of this branch; this is only
                        // defensive against a future scheduler implementation.
                        next_step_target = Some(advance_step_deadline(fired_at, period, 0));
                        continue;
                    }
                    active_timeline.get_or_insert(fired_at.timeline());
                    next_step_target = Some(advance_step_deadline(target, period, missed_ticks));

                    let now = match self.clock.read() {
                        ClockReading::Synchronized(now) if now.timeline() == target.timeline() => now,
                        ClockReading::Synchronized(_) => {
                            // The clock feed can replace the world history after the
                            // scheduler resolves but before this read. Let the
                            // higher-priority simulation-time arm install the
                            // ingress barrier and run Participant::reset before any step on
                            // the new timeline.
                            continue;
                        }
                        ClockReading::Unsynchronized(reason) => {
                            // Do not freeze, and do not hold on hoping it comes
                            // back: a frozen participant is what leaves an actuator
                            // commanded, and there is no uncertainty estimator that
                            // could justify a grace window. The participant fails
                            // now, teardown parks the hardware, and the supervisor's
                            // ordinary restart policy decides what happens next.
                            tracing::error!(
                                target: "phoxal.runtime",
                                error = %reason,
                                "clock discipline lost; failing the participant"
                            );
                            return LoopExit::ClockDisciplineLost(reason);
                        }
                    };
                    let dt = last_step_at
                        .and_then(|last| now.duration_since(last).ok())
                        .unwrap_or_default();
                    last_step_at = Some(now);

                    let step = StepContext {
                        token: StepToken::mint(now),
                        step_index,
                        dt,
                        missed_ticks,
                    };
                    step_index += 1;

                    // A handler error is terminal. A scheduled transition owns
                    // the participant's mutable state, so continuing after an
                    // error would make the Ready claim untrustworthy.
                    let observation = self
                        .runtime_performance
                        .begin_step(target, fired_at, missed_ticks);
                    let success = match self.participant.step(&self.api, step, &mut self.state) {
                        Ok(()) => true,
                        Err(e) => {
                            self.runtime_performance.finish_step(observation, false);
                            return LoopExit::StepFailed(e);
                        }
                    };
                    self.runtime_performance.finish_step(observation, success);
                }
                request = next_query(&mut self.queries) => {
                    if let Err(error) = self.serve_query(request) {
                        return LoopExit::QueryDispatchFailed(error);
                    }
                }
            }
        }
    }

    fn serve_query(&mut self, request: (usize, phoxal_bus::IncomingQuery)) -> crate::Result<()> {
        let Some(queries) = &self.queries else {
            return Ok(());
        };
        queries.serve(request, &self.participant, &self.api, &mut self.state)
    }
}

/// Subscribe the authoritative `simulation/clock` feed (published by the
/// `Simulator` kind that owns the world, e.g. the Webots controller) and drive
/// `handle` from it for the lifetime of the returned task.
///
/// It subscribes the same global `simulation/clock` wire key every sim
/// participant on the robot observes
/// (`api::topic::client().simulation().clock()`, the CLIENT side of the
/// `Simulator`'s owner-side publish - both sides format the identical
/// `simulation/clock` key), then per received sample:
///
/// - reads the exact production instant from the envelope - the world
///   authority stamped it with a world step token - and advances the scheduler
///   with it;
/// - accepts any different timeline, since timelines are opaque identities with
///   no generation order, while ignoring clocks from recently retired ones.
///
/// Every received sample represents one completed world advance. If the
/// simulator stops publishing, logical time simply stops advancing; no
/// separate pause flag is needed.
///
/// The task owns `handle` for its whole lifetime and runs until the
/// subscriber's underlying bus session closes. The runner registers it as a
/// `Critical` managed task before setup, then cancels and joins it through the
/// ordinary teardown sequence. Nothing else drives `handle` once this task is
/// spawned, so keeping it running for the loop's duration is what keeps the
/// scheduler advancing - the simulation scheduler separately retains its own
/// sender keepalive (see its docs), so the watch channel itself would not close
/// even if this task stopped, but a stopped task means logical time simply never
/// advances again.
async fn simulation_clock_feed(bus: BusHandle, handle: SimulationClockHandle) -> crate::Result<()> {
    let topic = api::topic::client().simulation().clock();
    let subscriber = match Subscriber::<api::simulation::Clock>::new(&bus, &topic).await {
        Ok(subscriber) => subscriber,
        Err(error) => return Err(error.into()),
    };
    tracing::info!(
        target: "phoxal.runtime",
        topic = topic.key(),
        "subscribed the live simulation/clock feed; driving the simulation scheduler from it"
    );
    loop {
        let observed = subscriber
            .recv()
            .await
            .map_err(|error| anyhow::anyhow!("simulation/clock subscriber terminated: {error}"))?;
        let Some(at) = observed.metadata.produced_exactly_at() else {
            return Err(anyhow::anyhow!(
                "simulation/clock sample has no exact production instant"
            ));
        };
        match handle.advance(at) {
            SimulationClockAdvance::Advanced | SimulationClockAdvance::DuplicateOrBackward => {}
            SimulationClockAdvance::RetiredTimeline => {
                tracing::warn!(
                    target: "phoxal.runtime",
                    timeline = %at.timeline(),
                    ticks = at.ticks(),
                    "ignoring late simulation clock from a retired world history"
                );
            }
        }
    }
}

/// Resolve on the next request when the participant declared a query surface,
/// and never when it did not.
async fn next_query<R: Participant>(
    queries: &mut Option<QuerySurface<R>>,
) -> (usize, phoxal_bus::IncomingQuery) {
    match queries {
        Some(queries) => queries.next_request().await,
        None => std::future::pending().await,
    }
}

/// Resolve on the next logical-time change when this participant observes one,
/// and never when it does not.
async fn simulation_time_change(
    receiver: &mut Option<tokio::sync::watch::Receiver<Option<RobotInstant>>>,
) -> RobotInstant {
    let Some(receiver) = receiver else {
        return std::future::pending().await;
    };
    loop {
        if receiver.changed().await.is_ok() {
            if let Some(at) = *receiver.borrow_and_update() {
                return at;
            }
            continue;
        }
        // A simulation scheduler retains its sender for the runner lifetime.
        // If a future implementation closes it, disable this branch instead
        // of spinning.
        std::future::pending::<()>().await;
    }
}

/// The instant the step after the one due at `target` is due at: one period on,
/// plus one for each period a released tick collapsed.
fn advance_step_deadline(
    target: RobotInstant,
    period: Duration,
    missed_ticks: u32,
) -> RobotInstant {
    target.saturating_add(period.saturating_mul(missed_ticks.saturating_add(1)))
}

fn retain_timeline(retentions: &[TimelineRetention], timeline: TimelineId) {
    for retention in retentions {
        retention(timeline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(timeline: u64, ticks: u64) -> RobotInstant {
        RobotInstant::new(
            TimelineId::from_raw(timeline).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    #[tokio::test]
    async fn shutdown_request_remains_sticky_after_source_completes() {
        let mut shutdown = ShutdownController::new(std::future::ready(()));
        shutdown.wait().await;
        assert!(shutdown.is_requested());
        tokio::time::timeout(Duration::from_millis(10), shutdown.wait())
            .await
            .expect("a completed shutdown source must remain immediately observable");
    }

    #[test]
    fn step_deadlines_skip_collapsed_ticks_and_saturate_instead_of_wrapping() {
        assert_eq!(
            advance_step_deadline(at(1, 10), Duration::from_nanos(10), 3),
            at(1, 50),
            "target 10 plus the fired period and 3 collapsed periods should resume at 50"
        );
        assert_eq!(
            advance_step_deadline(at(2, u64::MAX - 2), Duration::from_nanos(10), 3),
            at(2, u64::MAX)
        );
    }

    /// Both failing loop exits reach the operator as an actionable error naming
    /// what went wrong, and the clock one keeps its reason as a value rather
    /// than only as rendered text.
    #[test]
    fn loop_exits_report_actionable_failures() {
        let clock = LoopExit::ClockDisciplineLost(TimeUnsynchronized::ClockFault)
            .into_result()
            .expect_err("lost clock discipline is a failure");
        assert_eq!(
            clock
                .downcast_ref::<ClockDisciplineLost>()
                .map(|lost| lost.reason),
            Some(TimeUnsynchronized::ClockFault),
            "the reason must survive as a value, not only in the message: {clock}"
        );
        assert_eq!(
            format!("{clock}"),
            "clock discipline lost: the host boot clock read failed or regressed",
            "the supervisor keeps this text as the failure evidence"
        );

        let panicked = LoopExit::ManagedTaskFaulted(ManagedTaskExit {
            name: "io-pump".to_string(),
            panic_message: Some("serial port vanished".to_string()),
            error_message: None,
        })
        .into_result()
        .expect_err("a faulted managed task is a failure");
        assert_eq!(
            format!("{panicked}"),
            "managed task \"io-pump\" panicked: serial port vanished"
        );

        let returned = LoopExit::ManagedTaskFaulted(ManagedTaskExit {
            name: "io-pump".to_string(),
            panic_message: None,
            error_message: None,
        })
        .into_result()
        .expect_err("a faulted managed task is a failure");
        assert_eq!(
            format!("{returned}"),
            "managed task \"io-pump\" exited unexpectedly"
        );

        assert!(LoopExit::ShutdownRequested.into_result().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_wrong_running_executable_digest_is_rejected_before_bus_open() {
        let error = verify_current_executable(Sha256Digest::of(b"not-this-process"))
            .expect_err("a substituted executable must fail local startup");
        let message = format!("{error:#}");
        assert!(message.contains("does not match the runtime bundle binary digest"));
        assert!(message.contains("expected"));
        assert!(message.contains("got"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_executable_identity_fails_before_bus_open() {
        let error = verify_current_executable(Sha256Digest::of(b"ignored"))
            .expect_err("supervised launch must fail closed on unsupported targets");
        assert!(
            error
                .downcast_ref::<UnsupportedSecureExecutableIdentification>()
                .is_some(),
            "the failure must identify the secure executable primitive as unsupported"
        );
        assert!(format!("{error:#}").contains("supervised launch is disabled"));
    }

    /// A Critical task that fails while the bus-side Ready declaration is
    /// awaiting must win the boundary race, so no Ready token is accepted.
    #[tokio::test(start_paused = true)]
    async fn ready_declaration_race_prefers_a_task_failure() {
        let (trigger, triggered) = tokio::sync::oneshot::channel();
        let mut tasks = ManagedTasks::default();
        tasks.spawn(
            "declaration-race",
            ManagedTaskPolicy::Critical,
            async move {
                triggered.await.expect("the declaration triggers the task");
                Err::<(), _>(anyhow::anyhow!(
                    "setup task failed during Ready declaration"
                ))
            },
        );

        let declaration = async move {
            trigger.send(()).expect("the task is still supervised");
            tokio::task::yield_now().await;
            Ok::<(), ()>(())
        };
        let failure = tokio::select! {
            biased;
            exit = tasks.next_unexpected_exit() => Some(exit),
            _ = declaration => None,
        };
        let failure = failure.expect("task failure must preempt Ready acquisition");
        assert_eq!(failure.name, "declaration-race");
        assert_eq!(
            failure.error_message.as_deref(),
            Some("setup task failed during Ready declaration")
        );
    }

    /// A stop received while setup is still awaiting must cancel setup-owned
    /// tasks and return before the Ready boundary. This exercises the same
    /// `Runner::start` path used by a supervised participant, with an in-memory
    /// bus owner so the test can prove that the owner remains available for
    /// cleanup and that no Ready token is produced.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_during_hanging_setup_never_reaches_ready() {
        #[phoxal::service(id = "hanging-startup", state = ())]
        struct HangingStartup;

        impl Participant for HangingStartup {
            async fn setup(
                &self,
                _ctx: &mut SetupContext<Self>,
                _config: Self::Config,
            ) -> crate::Result<(Self::State, Self::Api)> {
                std::future::pending().await
            }
        }

        let participant_id = ParticipantId::new("hanging-startup").expect("valid participant id");
        let (owner, bus) = BusOwner::open(BusConfig::for_participant(
            phoxal_runtime_contract::identity::ExecutionId::mint(),
            participant_id.clone(),
            Vec::new(),
        ))
        .await
        .expect("open in-process bus");
        let (scheduler, clock_handle) = AnyStepScheduler::for_clock_mode(
            ClockMode::Real,
            None,
            Some(RobotInstant::new(
                TimelineId::from_raw(1).expect("valid timeline"),
                0,
            )),
        )
        .expect("real scheduler");
        assert!(clock_handle.is_none());
        let (bus_logs, bus_log_task) = bus_log::attach(bus.clone(), participant_id.as_str());
        let clock = RealClock::new(phoxal_runtime_contract::origin::ExecutionOrigin::mint())
            .expect("current-boot origin");
        let mut owner = Some(owner);
        let mut shutdown = ShutdownController::new(std::future::ready(()));
        let result = Runner::<HangingStartup, RealClock>::start(
            &bus,
            &mut owner,
            &participant_id,
            Duration::from_millis(100),
            &mut shutdown,
            None,
            (),
            RunnerClock::Delegated(clock),
            scheduler,
            None,
            ClockMode::Real,
            RunnerTasks {
                simulation_clock: None,
                bus_log: bus_log_task,
                query_reply_delay: None,
            },
        )
        .await;
        let StartOutcome::Terminal { result, deadline } = result else {
            panic!("shutdown during setup must terminate before Ready");
        };
        result.expect("startup cancellation should be clean");
        assert!(
            owner.is_some(),
            "startup cleanup must leave close ownership intact"
        );
        bus_logs.shutdown();
        owner
            .take()
            .expect("owner retained for cleanup")
            .close_until(deadline.instant())
            .await
            .expect("bus close after cancelled setup");
    }
}
