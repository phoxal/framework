//! The runner (D23/D34): owns the bus connection, the clock, step scheduling,
//! server-query dispatch, snapshot commits, and graceful shutdown.
//!
//! `phoxal::run::<R>()` builds a blocking Tokio runtime and runs the participant to
//! completion; `phoxal::tokio::run::<R>().await` is the async entrypoint for
//! custom Tokio mains.
//!
//! Serving model (D16): exclusive `#[server]` queries are awaited on the main
//! task (holding `&mut self` and `&mut Self::Api`, serialized with `#[step]`);
//! concurrent `#[server_snapshot]` queries are spawned and read a committed
//! `Snapshot`. A snapshot is committed after `#[setup]`, after each `#[step]`,
//! and after each exclusive `#[server]`.
//!
//! # `Api` ownership (D3: "read-only `&Self::Api`, or an api snapshot")
//!
//! `#[setup]` returns `(participant, api)` as two independent values
//! (`ParticipantLifecycle::__setup`). This runner keeps:
//!
//! - **`api: R::Api`**, owned directly (not behind `Arc`) - passed as
//!   `&mut Self::Api` to `#[step]`/exclusive `#[server]`/`#[shutdown]`, all
//!   awaited serially on the main task (same exclusivity rule as `#[step]`/
//!   `#[server]`, D16), so a plain owned value always gives a sound `&mut`
//!   with no synchronization needed;
//! - **`api_shared: Arc<R::Api>`**, one clone of `api` made right after
//!   `#[setup]` returns, handed to every spawned `#[server_snapshot]` task
//!   (`Arc::clone`, cheap) for the participant's whole lifetime.
//!
//! These are **two independent `Clone` instances of the same handle set**,
//! not one value shared behind both `&mut` and `Arc` at once (which Rust's
//! aliasing rules forbid without unsafe code - not used anywhere in this
//! module). Every `ParticipantApi` field type is `Clone` precisely because
//! every real operation on it takes `&self`
//! (`phoxal-bus/src/handle.rs`'s `Publisher`/`Querier`/`Latest`/`Subscriber`
//! `Clone` impls, [`Server`](super::api::Server)'s `Clone`/`Copy` impl, and
//! [`ParticipantApi`](super::api::ParticipantApi)'s own docs): a clone is a
//! second handle to the same underlying `Bus`/subscription/session.
//!
//! For **`Publisher`, `Latest`, `Querier`, and `Server` this is fully sound
//! AND behaviorally exact**: their operations are non-destructive reads or
//! fresh-envelope publishes (`Latest::latest()` clones the retained `Arc`
//! under its mutex and clones the body after releasing that lock,
//! `Publisher`/`Querier` build a new envelope per call, and `Server` carries
//! no live connection), so `api` and every `api_shared` clone always observe
//! and produce the identical live state - they can never diverge. That is
//! D3's "an api snapshot", realized through shared handles without wrapping
//! the whole `Api` in a `RwLock` or using `unsafe`.
//!
//! **`Subscriber` is the one exception, and it constrains snapshot-server
//! code.** A `Subscriber<B>`'s backing `Ring` is a single shared
//! `Mutex<VecDeque>` behind one `Arc`, and `recv`/`try_recv` *pop* from it
//! (`phoxal-bus/src/handle.rs`). So the owned `api` and the `Arc<R::Api>`
//! snapshot clone hold two handles to **one** queue: if BOTH sides drained
//! it, buffered samples would be split between them (each sample delivered to
//! exactly one caller), not duplicated - silent message loss, no panic. This
//! runner never does that: `#[step]`/exclusive `#[server]`/`#[shutdown]` own
//! the `&mut api` and are the only place a `Subscriber` should be `recv`'d,
//! while `#[server_snapshot]` handlers get the read-only `Arc` snapshot and
//! **must read committed `Snapshot` state, never `recv` a `Subscriber`**
//! (draining a subscription from a concurrent snapshot server is an
//! anti-pattern - see `Subscriber`'s and `Subscriber::recv`'s rustdoc).
//! **Deferred guard:** this rule is documentation-only for now - a
//! compile-time reject of a `#[server_snapshot]` handler that `recv`s a
//! `Subscriber` field would need the snapshot codegen to see the `Api` field
//! kinds (which it does not today), so it is left as a hardening follow-up
//! rather than an enforced invariant in this slice.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::api;
use crate::bus::Subscriber;
use crate::bus::{LocalInstant, QueryFailure, RobotInstant, StepToken, TimelineId};
use crate::participant::api::{ParticipantApi, ParticipantLifecycle};
use crate::participant::bus_log::{self, BusLogState};
use crate::participant::clock::{ClockReading, ClockSource, RealClock, TimeUnsynchronized};
use crate::participant::context::{ResetContext, SetupContext, ShutdownContext, StepContext};
use crate::participant::launch::{ClockMode, ParticipantLaunch, ParticipantLaunchPolicy};
use crate::participant::managed::{ManagedTaskExit, ManagedTasks};
use crate::participant::runtime_performance::{RuntimePerformance, RuntimePerformancePublisher};
use crate::participant::scheduler::{
    AnyStepScheduler, RealScheduler, SchedulerTick, SimulationClockAdvance, SimulationClockHandle,
    SimulationScheduler, StepScheduler, duration_to_nanos_saturating,
};
use crate::participant::spec::StepSchedule;
use anyhow::Context as _;
use phoxal_bus::{Bus, BusConfig, IncomingQuery};

const RUNTIME_PERFORMANCE_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// The participant macro's launch policy decides whether clock selection exists
/// at all: services/drivers are selectable, while tools and simulators are
/// structurally host-driven. The default binary entrypoint is
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
pub fn run<R: ParticipantLifecycle>() -> crate::Result<()> {
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tokio_runtime.block_on(run_async::<R>())
}

/// Async host runner for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
pub async fn run_async<R: ParticipantLifecycle>() -> crate::Result<()> {
    let launch = R::LaunchPolicy::from_cli(R::ID, "robot")?;

    init_tracing();

    run_with::<R, _>(launch, shutdown_signal()).await
}

/// Select the step scheduler for `clock_mode` (D34/#09): the seam that
/// answers "when should the next `#[step]` tick fire", separate from the
/// [`ClockSource`] used for timestamps.
///
/// [`ClockMode::Real`] preserves the runner's pre-#09 wall-clock cadence
/// exactly, returning no driving handle (`None`). [`ClockMode::Simulation`]
/// starts a [`SimulationScheduler`] and returns its [`SimulationClockHandle`]
/// (`Some`) so the caller can wire the live `simulation/clock` subscription
/// (see [`spawn_simulation_clock_feed`]): the handle is the attachment point
/// anything that produces a `RobotInstant` drives the scheduler through (a bus
/// subscription task, a test, a REPL).
pub(crate) fn step_scheduler_for(
    clock_mode: ClockMode,
    schedule: Option<StepSchedule>,
    now: Option<RobotInstant>,
) -> crate::Result<(AnyStepScheduler, Option<SimulationClockHandle>)> {
    let missed_tick = schedule
        .map(|s| s.missed_tick)
        .unwrap_or(crate::participant::spec::MissedTick::Collapse);
    let period = schedule.map(|s| s.period());
    Ok(match clock_mode {
        ClockMode::Real => {
            // A real participant has no instant to anchor its cadence on until
            // the clock is trustworthy. Anchoring on an invented timeline would
            // publish a world history nobody authored, so the participant does
            // not start at all - which is the ordinary failure the supervisor
            // already knows how to handle.
            let now = now.context(
                "a real participant cannot anchor its cadence without a synchronized clock",
            )?;
            (
                AnyStepScheduler::Real(
                    RealScheduler::new(missed_tick, period, now)
                        .context("the host boot clock could not be read to anchor cadence")?,
                ),
                None,
            )
        }
        ClockMode::Simulation => {
            // No seed at all: there is no world history until the authority
            // publishes one, and inventing instant zero of an invented timeline
            // is exactly the sentinel this train deletes.
            let (scheduler, handle) = SimulationScheduler::new(missed_tick, period);
            (AnyStepScheduler::Simulation(scheduler), Some(handle))
        }
        // A clockless participant has no cadence and no clock feed to
        // subscribe: it is driven by host events or by the simulator that owns
        // it, and it expresses no robot time.
        ClockMode::Clockless => (AnyStepScheduler::Clockless, None),
    })
}

/// Subscribe the authoritative `simulation/clock` feed (published by the
/// `Simulator` kind that owns the world, e.g. the Webots controller) and drive
/// `handle` from it for the lifetime of the returned task.
///
/// Mirrors the snapshot-server task pattern (bus-driven task, pushed alongside
/// the other server tasks, aborted at shutdown): this subscribes the same
/// global `simulation/clock` wire key every sim participant on the robot
/// observes (`api::topic::client().simulation().clock()`, the CLIENT side of
/// the `Simulator`'s owner-side publish - both sides format the identical
/// `simulation/clock` key, D61/D62), then per received sample:
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
/// subscriber's underlying bus session closes; the caller aborts it
/// explicitly at shutdown alongside the other server tasks. Nothing else
/// drives `handle` once this task is spawned, so keeping it running for the
/// loop's duration is what keeps the scheduler advancing - the
/// `SimulationScheduler` separately retains its own sender keepalive (see its
/// docs), so the watch channel itself would not close even if this task
/// stopped, but a stopped task means logical time simply never advances
/// again.
pub(crate) fn spawn_simulation_clock_feed(
    bus: &Bus,
    handle: SimulationClockHandle,
) -> crate::Result<JoinHandle<()>> {
    let bus = bus.clone();
    Ok(tokio::spawn(async move {
        let topic = api::topic::client().simulation().clock();
        let subscriber = match Subscriber::<api::simulation::Clock>::new(&bus, &topic, 1).await {
            Ok(subscriber) => subscriber,
            Err(error) => {
                tracing::error!(
                    target: "phoxal.runtime",
                    error = %error,
                    "failed to subscribe simulation/clock; simulation-mode steps will never advance"
                );
                return;
            }
        };
        tracing::info!(
            target: "phoxal.runtime",
            topic = topic.key(),
            "subscribed the live simulation/clock feed; driving the simulation scheduler from it"
        );
        while let Ok(observed) = subscriber.recv().await {
            let Some(at) = observed.metadata.produced_exactly_at() else {
                tracing::warn!(
                    target: "phoxal.runtime",
                    "discarding a simulation clock with no exact production instant"
                );
                continue;
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
    }))
}

/// Run a participant against an explicit launch and shutdown trigger. The
/// runner owns the host clock; tools therefore have no clock parameter to
/// receive or override.
pub async fn run_with<R, S>(launch: ParticipantLaunch, shutdown: S) -> crate::Result<()>
where
    R: ParticipantLifecycle,
    S: Future<Output = ()>,
{
    init_tracing();

    let bus = Bus::open(BusConfig {
        namespace: launch.namespace.clone(),
        robot_id: launch.robot_id.clone(),
        execution: launch.execution,
        participant: launch.participant_id.clone(),
        producer: launch.producer,
        connect_endpoints: launch.bus.connect_endpoints.clone(),
    })
    .await?;

    let result = run_with_bus::<R, S>(&bus, launch, shutdown).await;

    if let Err(e) = bus.close().await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "bus close failed");
    }
    result
}

/// Run a participant on a **caller-owned** bus, against an explicit launch and
/// shutdown trigger. Unlike [`run_with`], this does not open or close the bus - the
/// caller controls its lifecycle.
///
/// This is the embedding seam for co-locating participants on a single in-process
/// [`Bus`] (a single-process simulation, or an integration test exercising
/// participant-to-participant data flow over a shared session). Note that bus metadata
/// `source` identity is a property of the *bus*, not the launch: participants
/// sharing one [`Bus`] publish under that bus's participant id, so distinct
/// per-participant source attribution still requires a bus per participant. The
/// `launch` here drives config, robot-model, and component-instance resolution.
pub async fn run_with_bus<R, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantLifecycle,
    S: Future<Output = ()>,
{
    let clock = match launch.execution_origin {
        Some(origin) => RealClock::new(origin),
        None => RealClock::without_origin(),
    };
    run_with_bus_inner::<R, RealClock, S>(bus, launch, clock, shutdown).await
}

/// Deterministic clock-injection seam for clock-selectable checked graph
/// participants. Fixed tool and simulator launch policies exclude them even if
/// user code manually adds the public
/// [`TypedGraphSurface`](crate::participant::TypedGraphSurface) marker.
#[doc(hidden)]
pub async fn run_with_bus_clock<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantLifecycle<LaunchPolicy = crate::participant::launch::ClockedParticipantLaunch>
        + crate::participant::TypedGraphSurface,
    C: ClockSource,
    S: Future<Output = ()>,
{
    run_with_bus_inner::<R, C, S>(bus, launch, clock, shutdown).await
}

async fn run_with_bus_inner<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantLifecycle,
    C: ClockSource,
    S: Future<Output = ()>,
{
    init_tracing();

    let participant_id = launch.participant_id.clone();
    let bus_logs = bus_log::attach(bus.clone(), &participant_id);
    let result = run_lifecycle::<R, C, S>(bus, launch, clock, shutdown).await;
    bus_logs.shutdown().await;
    result
}

async fn run_lifecycle<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantLifecycle,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let schedule = R::__step_schedule();
    let clock_mode = R::LaunchPolicy::clock_mode(&launch);
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
        // start-limit policy decides what happens next (#952 section J).
        return Err(clock_discipline_error(reason));
    }
    let (scheduler, clock_handle) = step_scheduler_for(clock_mode, schedule, reading.instant())?;
    let effective_clock = Arc::new(match &scheduler {
        AnyStepScheduler::Simulation(sim) => RunnerClock::Simulation(sim.simulation_clock()),
        AnyStepScheduler::Real(_) | AnyStepScheduler::Clockless => RunnerClock::Delegated(clock),
    });
    // Subscribe before setup so the simulation clock can advance while setup runs.
    let clock_feed = clock_handle
        .map(|handle| spawn_simulation_clock_feed(bus, handle))
        .transpose()?;

    let result = run_lifecycle_inner::<R, C, S>(
        bus,
        launch,
        Arc::clone(&effective_clock),
        scheduler,
        schedule,
        shutdown,
    )
    .await;
    if let Some(task) = clock_feed {
        task.abort();
    }
    result
}

/// The runner's effective timestamp clock, chosen once the scheduler is built.
///
/// In real mode it delegates to the runner-owned host [`RealClock`] (or a
/// [`TestClock`](crate::participant::clock::TestClock) through the checked-graph
/// test seam). In simulation mode it is the
/// [`SimulationClock`](crate::participant::clock::SimulationClock) that shares
/// the `SimulationScheduler`'s live `simulation/clock` feed, so stamped time and
/// step-release time stay in the one simulation domain (see the `SimulationClock`
/// docs for why wall-stamping a simulated participant is wrong).
enum RunnerClock<C: ClockSource> {
    Delegated(C),
    Simulation(crate::participant::clock::SimulationClock),
}

impl<C: ClockSource> ClockSource for RunnerClock<C> {
    fn read(&self) -> ClockReading {
        match self {
            RunnerClock::Delegated(clock) => clock.read(),
            RunnerClock::Simulation(clock) => clock.read(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_lifecycle_inner<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: Arc<RunnerClock<C>>,
    scheduler: AnyStepScheduler,
    schedule: Option<StepSchedule>,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantLifecycle,
    C: ClockSource,
    S: Future<Output = ()>,
{
    R::__validate_server_topics().map_err(anyhow::Error::msg)?;

    let config: R::Config = match &launch.config {
        Some(value) => serde_json::from_value(value.clone())?,
        None => serde_json::from_value(serde_json::Value::Null)?,
    };

    // Load the resolved robot model from the root, if one was provided, so
    // official participants can read it via `ctx.robot()` (D33).
    let robot = match &launch.robot_root {
        Some(root) => Some(Arc::new(crate::model::v0::Robot::read_from_dir(root)?)),
        None => None,
    };

    let mut ctx = SetupContext::<R>::new(
        bus.clone(),
        robot,
        launch.robot_root.clone(),
        launch.component_instance.clone(),
    );
    let (mut participant, api) = match R::__setup(&mut ctx, config).await {
        Ok(pair) => pair,
        Err(error) => {
            let grace = Duration::from_millis(launch.shutdown_grace_ms);
            let unjoined = ctx.take_managed_tasks().shutdown_within(grace).await;
            log_unjoined_managed_tasks(unjoined, launch.shutdown_grace_ms);
            return Err(error);
        }
    };
    // Managed tasks spawned via `ctx.spawn_managed(...)` during `#[setup]` (D-managed-tasks):
    // from here on the runner - not `SetupContext` - owns watching them for an
    // unexpected exit and cancelling/joining them at shutdown.
    let mut managed_tasks = ctx.take_managed_tasks();

    // The Api ownership split (see module docs): `api` stays owned for the
    // exclusive `&mut Self::Api` path; `api_shared` is the one clone every
    // concurrent `#[server_snapshot]` task gets its own `Arc::clone` of.
    let api_shared: Arc<R::Api> = Arc::new(api.clone());
    let mut api = api;

    // Committed snapshot, shared with concurrent snapshot-server tasks (D16).
    let committed: Arc<ArcSwapOption<R::Snapshot>> = Arc::new(ArcSwapOption::empty());
    commit_snapshot::<R>(&participant, &committed);

    // Forward exclusive-server queries to the main loop; keep one sender alive so
    // the receiver pends (never returns `None`) when there are no servers.
    let (excl_tx, mut excl_rx) = mpsc::channel::<IncomingQuery>(64);
    let mut server_tasks: Vec<JoinHandle<()>> = Vec::new();

    for topic in R::__exclusive_server_topics() {
        let queryable = match bus.declare_server(topic).await {
            Ok(queryable) => queryable,
            Err(error) => {
                teardown_lifecycle(
                    &mut participant,
                    &mut api,
                    server_tasks,
                    managed_tasks,
                    launch.shutdown_grace_ms,
                )
                .await;
                return Err(error.into());
            }
        };
        let tx = excl_tx.clone();
        server_tasks.push(tokio::spawn(async move {
            while let Ok(incoming) = queryable.recv().await {
                if tx.send(incoming).await.is_err() {
                    break;
                }
            }
        }));
    }

    // Concurrent snapshot-server queries run against the latest committed
    // snapshot. Each topic's per-query tasks live in a `JoinSet` owned by that
    // topic's task, so aborting the topic task on shutdown also aborts any
    // in-flight handlers (they never outlive the runner / race `bus.close`).
    for topic in R::__snapshot_server_topics() {
        let queryable = match bus.declare_server(topic).await {
            Ok(queryable) => queryable,
            Err(error) => {
                teardown_lifecycle(
                    &mut participant,
                    &mut api,
                    server_tasks,
                    managed_tasks,
                    launch.shutdown_grace_ms,
                )
                .await;
                return Err(error.into());
            }
        };
        let committed = Arc::clone(&committed);
        let api_shared = Arc::clone(&api_shared);
        let bus = bus.clone();
        server_tasks.push(tokio::spawn(async move {
            let mut inflight = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = queryable.recv() => {
                        let Ok(incoming) = incoming else { break };
                        let snapshot = committed.load_full();
                        let api = Arc::clone(&api_shared);
                        let bus = bus.clone();
                        inflight.spawn(async move {
                            serve_snapshot_query::<R>(&bus, incoming, snapshot, api).await
                        });
                    }
                    // Reap finished handlers so the JoinSet does not grow unbounded.
                    Some(_) = inflight.join_next() => {}
                }
            }
        }));
    }

    // Portable runtime evidence is measured at the runner-owned step/buffer
    // boundaries. No OS sampler or participant-authored telemetry is involved.
    let runtime_performance_publisher = RuntimePerformancePublisher::attach(bus.clone());
    let mut runtime_performance = RuntimePerformance::new(schedule);

    let shutdown = pin!(shutdown);
    let liveliness = match bus.declare_participant_liveliness().await {
        Ok(token) => token,
        Err(error) => {
            teardown_lifecycle(
                &mut participant,
                &mut api,
                server_tasks,
                managed_tasks,
                launch.shutdown_grace_ms,
            )
            .await;
            return Err(error.into());
        }
    };
    tracing::info!(target: "phoxal.runtime", id = R::ID, participant = %launch.participant_id, "runtime ready");
    let loop_result = main_loop::<R, _, S>(
        &mut participant,
        &mut api,
        bus,
        clock.as_ref(),
        &scheduler,
        schedule,
        &committed,
        &mut excl_rx,
        shutdown,
        &runtime_performance_publisher,
        &mut runtime_performance,
        &mut managed_tasks,
    )
    .await;
    drop(excl_tx);
    teardown_lifecycle(
        &mut participant,
        &mut api,
        server_tasks,
        managed_tasks,
        launch.shutdown_grace_ms,
    )
    .await;
    drop(liveliness);

    let fault = loop_result?;
    if let Some(fault) = fault {
        return Err(fault.into_error());
    }
    Ok(())
}

/// One shutdown path shared by normal completion and every fallible operation
/// after `#[setup]` succeeds. Keeping this sequence centralized prevents a
/// server-declaration error from bypassing the participant's hardware-safety
/// hook or detaching server/managed tasks before the bus closes.
async fn teardown_lifecycle<R>(
    participant: &mut R,
    api: &mut R::Api,
    server_tasks: Vec<JoinHandle<()>>,
    mut managed_tasks: ManagedTasks,
    shutdown_grace_ms: u64,
) where
    R: ParticipantLifecycle,
{
    for task in server_tasks {
        task.abort();
    }

    let grace = Duration::from_millis(shutdown_grace_ms);
    let shutdown_deadline = tokio::time::Instant::now() + grace;
    managed_tasks.cancel();

    // Bound the shutdown hook by the grace deadline (D24/D43i): a hook that
    // parks/flushes hardware can hang, but the runner must still proceed to
    // bus close deterministically rather than leak the process. On timeout we
    // log and move on; the hook's task is dropped (cancelled at the next await).
    let shutdown_remaining =
        shutdown_deadline.saturating_duration_since(tokio::time::Instant::now());
    match tokio::time::timeout(
        shutdown_remaining,
        participant.__shutdown(api, ShutdownContext::new(grace)),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(target: "phoxal.runtime", error = %error, "shutdown hook returned error");
        }
        Err(_elapsed) => {
            tracing::warn!(
                target: "phoxal.runtime",
                grace_ms = shutdown_grace_ms,
                "shutdown hook exceeded the grace deadline; proceeding to bus close"
            );
        }
    }

    // Join managed tasks before the bus closes (item 4/5/6 of the managed-task
    // contract): the same shutdown deadline bounds both `#[shutdown]` and
    // managed-task joining, so a stuck task cannot extend the grace window.
    let unjoined = managed_tasks.join_until(shutdown_deadline).await;
    log_unjoined_managed_tasks(unjoined, shutdown_grace_ms);
    tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");
}

/// Build the runtime-fault error for an unexpected `FaultOnExit` managed task
/// exit. Returned from `run_lifecycle_inner` through the participant result.
/// Why the main loop stopped without being asked to.
///
/// Both variants mean the same thing to the caller: run the ordinary teardown -
/// which parks the hardware through `#[shutdown]` - and then report a failure,
/// so the supervisor's restart and start-limit policy decides what happens next.
/// Neither is a distinct "failure mode" the participant handles itself.
pub(crate) enum LoopFault {
    /// A task the participant declared as fault-on-exit went away.
    ManagedTask(ManagedTaskExit),
    /// The clock stopped being trustworthy, so no further step could be timed.
    ClockDiscipline(TimeUnsynchronized),
}

impl LoopFault {
    fn into_error(self) -> anyhow::Error {
        match self {
            LoopFault::ManagedTask(exit) => managed_task_fault_error(&exit),
            LoopFault::ClockDiscipline(reason) => clock_discipline_error(reason),
        }
    }
}

/// The failure a participant reports when it cannot trust its own clock.
///
/// The reason is in the message because it is the operator's only handle on
/// which trigger fired: the supervisor captures this on the failing process and
/// keeps it in the failure evidence.
pub(crate) fn clock_discipline_error(reason: TimeUnsynchronized) -> anyhow::Error {
    anyhow::anyhow!("clock discipline lost: {reason}")
}

pub(crate) fn managed_task_fault_error(exit: &ManagedTaskExit) -> anyhow::Error {
    match &exit.panic_message {
        Some(message) => anyhow::anyhow!("managed task \"{}\" panicked: {message}", exit.name),
        None => anyhow::anyhow!("managed task \"{}\" exited unexpectedly", exit.name),
    }
}

pub(crate) fn log_unjoined_managed_tasks(unjoined: Vec<String>, grace_ms: u64) {
    if !unjoined.is_empty() {
        tracing::warn!(
            target: "phoxal.runtime",
            tasks = ?unjoined,
            grace_ms,
            "managed tasks were still running at the shutdown grace deadline"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn main_loop<R, C, S>(
    participant: &mut R,
    api: &mut R::Api,
    bus: &Bus,
    clock: &C,
    scheduler: &AnyStepScheduler,
    schedule: Option<StepSchedule>,
    committed: &Arc<ArcSwapOption<R::Snapshot>>,
    excl_rx: &mut mpsc::Receiver<IncomingQuery>,
    mut shutdown: std::pin::Pin<&mut S>,
    runtime_performance_publisher: &RuntimePerformancePublisher,
    runtime_performance: &mut RuntimePerformance,
    managed_tasks: &mut ManagedTasks,
) -> crate::Result<Option<LoopFault>>
where
    R: ParticipantLifecycle,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let period = schedule.map(|s| s.period());
    let mut step_index: u64 = 0;
    let mut active_timeline: Option<TimelineId> = None;
    let mut simulation_time_rx = scheduler.simulation_time_receiver();
    // The simulation clock feed starts before `#[setup]`. If setup takes long
    // enough for the authority's first world step to arrive, a newly-cloned
    // watch receiver sees that value as its initial state and has no change
    // notification to deliver. Establish that already-current world history
    // without invoking reset: there was no prior participant execution, but its
    // ingress barrier and first cadence still matter.
    let initial_time = scheduler.now();
    if let Some(initial_time) = initial_time.filter(|_| simulation_time_rx.is_some()) {
        active_timeline = Some(initial_time.timeline());
        api.__retain_timeline(initial_time.timeline());
    }
    let mut last_step_at = initial_time;
    // The next tick's *robot* due time - what the runner asks the scheduler to
    // release at (D34/#09), separate from the host-monotonic
    // runtime-performance publication tick below.
    let mut next_step_target =
        initial_time.and_then(|at| period.map(|period| advance_step_deadline(at, period, 0)));
    let mut next_runtime_performance_tick = tokio::time::Instant::now();

    loop {
        tokio::select! {
            // Order matters: shutdown first, then a managed-task fault (both are
            // "stop the loop" events and should preempt routine work), then the
            // runtime-performance publication tick, then a *due* step, then
            // server queries. Publication is cheap and must not be starved by
            // an overloaded participant; due steps still take priority over a
            // steady query backlog. `Some(..)`
            // disables the query branch if the channel ever closes, so it never
            // busy-loops.
            biased;
            _ = &mut shutdown => return Ok(None),
            exit = managed_tasks.next_unexpected_exit() => {
                tracing::error!(
                    target: "phoxal.runtime",
                    task = %exit.name,
                    panic = exit.panic_message.as_deref(),
                    "managed task exited unexpectedly; faulting the participant"
                );
                return Ok(Some(LoopFault::ManagedTask(exit)));
            }
            fired_at = simulation_time_change(&mut simulation_time_rx) => {
                if active_timeline == Some(fired_at.timeline()) {
                    continue;
                }

                // Timelines are opaque identities, not ordered generations. Any
                // different one establishes a replacement world history. This
                // branch is independent of `#[step]`, so clocked server-only
                // services receive the same serialized reset lifecycle.
                let previous_timeline = active_timeline.replace(fired_at.timeline());
                api.__retain_timeline(fired_at.timeline());
                if let Some(previous_timeline) = previous_timeline {
                    participant
                        .__reset(
                            api,
                            ResetContext::new(previous_timeline, fired_at.timeline()),
                        )
                        .await?;
                    commit_snapshot::<R>(participant, committed);
                }
                next_step_target =
                    period.map(|period| advance_step_deadline(fired_at, period, 0));
                step_index = 0;
                last_step_at = Some(fired_at);
                runtime_performance.reset(schedule);
            }
            _ = runtime_performance_tick(next_runtime_performance_tick) => {
                advance_deadline(
                    &mut next_runtime_performance_tick,
                    RUNTIME_PERFORMANCE_TICK_INTERVAL,
                );
                // A real participant with no `#[step]` schedule would otherwise
                // check its clock once at startup and never again, and go on
                // serving queries from state it can no longer date. This tick
                // is its only recurring beat, so clock discipline is checked
                // here too - a stepping participant reaches the same check
                // sooner, in its own step arm.
                //
                // Simulation is excluded on purpose: there, "unsynchronized"
                // means the world authority has not published a first step yet,
                // which is a world that has not started rather than a clock
                // that was lost.
                let faulted = LocalInstant::clock_faulted()
                    .then_some(TimeUnsynchronized::ClockFault)
                    .or_else(|| match (period, scheduler) {
                        (None, AnyStepScheduler::Real(_)) => match clock.read() {
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
                    return Ok(Some(LoopFault::ClockDiscipline(reason)));
                }
                if let Some(rollup) = runtime_performance.take_rollup(bus) {
                    runtime_performance_publisher.publish(rollup);
                }
            }
            SchedulerTick { fired_at, missed_ticks } = step_tick(scheduler, next_step_target) => {
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
                    return Ok(Some(LoopFault::ClockDiscipline(TimeUnsynchronized::ClockFault)));
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

                let now = match clock.read() {
                    ClockReading::Synchronized(now) if now.timeline() == target.timeline() => now,
                    ClockReading::Synchronized(_) => {
                        // The clock feed can replace the world history after the
                        // scheduler resolves but before this read. Let the
                        // higher-priority simulation-time arm install the
                        // ingress barrier and run #[reset] before any step on
                        // the new timeline.
                        continue;
                    }
                    ClockReading::Unsynchronized(reason) => {
                        // Do not freeze, and do not hold on hoping it comes
                        // back: a frozen participant is what leaves an actuator
                        // commanded, and there is no uncertainty estimator that
                        // could justify a grace window. The participant fails
                        // now, teardown parks the hardware, and the supervisor's
                        // ordinary restart policy decides what happens next
                        // (#952 section J).
                        tracing::error!(
                            target: "phoxal.runtime",
                            error = %reason,
                            "clock discipline lost; failing the participant"
                        );
                        return Ok(Some(LoopFault::ClockDiscipline(reason)));
                    }
                };
                let dt = last_step_at
                    .and_then(|last| now.duration_since(last).ok())
                    .unwrap_or_default();
                last_step_at = Some(now);

                let step = StepContext::new(
                    StepToken::__mint(now),
                    step_index,
                    dt,
                    missed_ticks,
                );
                step_index += 1;

                // A handler `Err` is a domain outcome: stay healthy, log, continue
                // (D32); the snapshot is committed only after a *successful* step so
                // a failed mutation is never published as committed state. A panic
                // would unwind and abort the process.
                let observation = runtime_performance.begin_step(target, fired_at, missed_ticks);
                let success = match participant.__step(api, step).await {
                    Ok(()) => {
                        commit_snapshot::<R>(participant, committed);
                        true
                    }
                    Err(e) => {
                        tracing::warn!(target: "phoxal.runtime", error = %e, "step returned error");
                        false
                    }
                };
                runtime_performance.finish_step(observation, success);
            }
            Some(incoming) = excl_rx.recv() => {
                // Commit only if the handler succeeded (D14/D32: retain the prior
                // snapshot on a handler error).
                if serve_exclusive_query::<R>(participant, api, bus, incoming).await {
                    commit_snapshot::<R>(participant, committed);
                }
            }
        }
    }
}

pub(crate) async fn simulation_time_change(
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

pub(crate) fn advance_step_deadline(
    target: RobotInstant,
    period: Duration,
    missed_ticks: u32,
) -> RobotInstant {
    let period_ns = duration_to_nanos_saturating(period);
    let periods = u64::from(missed_ticks).saturating_add(1);
    target.saturating_add(Duration::from_nanos(period_ns.saturating_mul(periods)))
}

pub(crate) async fn runtime_performance_tick(next: tokio::time::Instant) {
    tokio::time::sleep_until(next).await;
}

pub(crate) fn advance_deadline(next: &mut tokio::time::Instant, period: Duration) {
    *next += period;
    let now = tokio::time::Instant::now();
    while *next <= now {
        *next += period;
    }
}

/// Resolve when the scheduler releases the tick due at `target`; never
/// resolve when there is no step schedule (so the loop is driven only by
/// server queries / shutdown). This is the sole seam through which the main
/// loop asks "when should the next `#[step]` tick fire" (D34/#09) - real mode
/// sleeps on wall time, simulation mode waits on logical time, and the main
/// loop itself does not know which.
pub(crate) async fn step_tick(
    scheduler: &AnyStepScheduler,
    target: Option<RobotInstant>,
) -> SchedulerTick {
    match target {
        Some(target) => scheduler.wait_until(target).await,
        None => std::future::pending().await,
    }
}

fn commit_snapshot<R: ParticipantLifecycle>(
    participant: &R,
    committed: &Arc<ArcSwapOption<R::Snapshot>>,
) {
    if R::HAS_SNAPSHOT {
        committed.store(Some(Arc::new(participant.__take_snapshot())));
    }
}

/// Serve one exclusive query. Returns `true` iff the handler succeeded (so the
/// runner should commit a fresh snapshot).
async fn serve_exclusive_query<R: ParticipantLifecycle>(
    participant: &mut R,
    api: &mut R::Api,
    bus: &Bus,
    incoming: IncomingQuery,
) -> bool {
    let topic = incoming.topic_key().to_string();
    let metadata = match incoming.request_metadata() {
        Ok(m) => m,
        Err(e) => {
            let _ = incoming
                .reply_err(&QueryFailure::invalid_argument(e.to_string()))
                .await;
            return false;
        }
    };
    if metadata.codec_id().is_none() {
        let _ = incoming
            .reply_err(&QueryFailure::invalid_argument(format!(
                "unsupported request codec id {}",
                metadata.codec
            )))
            .await;
        return false;
    }
    let request = match incoming.request_bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = incoming
                .reply_err(&QueryFailure::invalid_argument(e.to_string()))
                .await;
            return false;
        }
    };
    match participant.__serve_exclusive(api, &topic, &request).await {
        Ok(reply) => {
            let _ = incoming.reply(bus, reply.payload).await;
            true
        }
        Err(failure) => {
            let _ = incoming.reply_err(&failure).await;
            false
        }
    }
}

/// Serve one concurrent `#[server_snapshot]` query, handing the generated
/// dispatcher its `Arc<R::Api>` clone (D3).
async fn serve_snapshot_query<R: ParticipantLifecycle>(
    bus: &Bus,
    incoming: IncomingQuery,
    snapshot: Option<Arc<R::Snapshot>>,
    api: Arc<R::Api>,
) {
    let topic = incoming.topic_key().to_string();
    let metadata = match incoming.request_metadata() {
        Ok(m) => m,
        Err(e) => {
            let _ = incoming
                .reply_err(&QueryFailure::invalid_argument(e.to_string()))
                .await;
            return;
        }
    };
    if metadata.codec_id().is_none() {
        let _ = incoming
            .reply_err(&QueryFailure::invalid_argument(format!(
                "unsupported request codec id {}",
                metadata.codec
            )))
            .await;
        return;
    }
    let request = match incoming.request_bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = incoming
                .reply_err(&QueryFailure::invalid_argument(e.to_string()))
                .await;
            return;
        }
    };
    let Some(snapshot) = snapshot else {
        let _ = incoming
            .reply_err(&QueryFailure::unavailable("no committed snapshot yet"))
            .await;
        return;
    };
    match R::__serve_snapshot(snapshot, api, topic, request).await {
        Ok(reply) => {
            let _ = incoming.reply(bus, reply.payload).await;
        }
        Err(failure) => {
            let _ = incoming.reply_err(&failure).await;
        }
    }
}

pub(crate) async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "failed to listen for ctrl-c");
    }
}

pub(crate) fn init_tracing() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let bus_layer = bus_log::BusLogLayer::new(bus_log_state());
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .with(bus_layer)
            .try_init();
    });
}

pub(crate) fn bus_log_state() -> Arc<BusLogState> {
    static STATE: OnceLock<Arc<BusLogState>> = OnceLock::new();
    Arc::clone(STATE.get_or_init(bus_log::new_state_from_env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{StatePublisher, TimelineAuthority};

    fn line(value: u64) -> TimelineId {
        TimelineId::from_raw(value).expect("test timeline must be nonzero")
    }

    fn at(timeline: u64, ticks: u64) -> RobotInstant {
        RobotInstant::new(line(timeline), ticks)
    }

    #[test]
    fn step_scheduler_for_selects_real_or_simulation_by_clock_mode() {
        let schedule = Some(StepSchedule::hz(100.0));

        let (real, real_handle) =
            step_scheduler_for(ClockMode::Real, schedule, Some(at(1, 0))).expect("real scheduler");
        assert!(matches!(real, AnyStepScheduler::Real(_)));
        assert!(
            real_handle.is_none(),
            "real mode has no simulation clock handle to drive"
        );

        let (simulation, simulation_handle) =
            step_scheduler_for(ClockMode::Simulation, schedule, None)
                .expect("simulation scheduler");
        assert!(matches!(simulation, AnyStepScheduler::Simulation(_)));
        assert!(
            simulation_handle.is_some(),
            "simulation mode must hand back the driving handle so the caller can wire the live feed"
        );
        assert_eq!(
            simulation.now(),
            None,
            "simulation mode starts with no world history, not with an invented zero"
        );
    }

    #[test]
    fn a_real_participant_with_no_trustworthy_clock_does_not_get_a_cadence_at_all() {
        // The alternative was anchoring the cadence on an invented timeline at
        // tick zero, which publishes a world history nobody authored. Refusing
        // to start is the ordinary failure the supervisor already handles.
        assert!(step_scheduler_for(ClockMode::Real, Some(StepSchedule::hz(50.0)), None).is_err());
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

    #[tokio::test]
    async fn simulation_scheduler_selected_by_the_runner_schedules_deterministically() {
        // Exercises the exact scheduler + handle `step_scheduler_for` selects
        // for `ClockMode::Simulation`, driven purely by robot time via its own
        // `SimulationClockHandle` - no real sleeping, no live bus/Webots feed.
        // This is the deterministic proof that simulation mode schedules ticks
        // from robot time (acceptance criterion); the full
        // `run_with`/`ClockMode::Simulation` integration path (live feed wiring
        // included) is covered by `tests/runner.rs`.
        let schedule = StepSchedule::hz(10.0); // 100ms period
        let period_ns = duration_to_nanos_saturating(schedule.period());

        let (scheduler, handle) = step_scheduler_for(ClockMode::Simulation, Some(schedule), None)
            .expect("simulation scheduler");
        let handle = handle.expect("simulation mode must hand back a driving handle");

        let mut fired = Vec::new();
        let mut target = at(1, period_ns);
        for _ in 0..3 {
            handle.advance(target);
            let tick = scheduler.wait_until(target).await;
            fired.push(tick.fired_at.ticks());
            target = at(1, target.ticks() + period_ns);
        }

        assert_eq!(
            fired,
            vec![100_000_000, 200_000_000, 300_000_000],
            "ticks fire in order at the instants the handle advanced to, with no real sleeping"
        );
    }

    /// Isolation-level proof of [`spawn_simulation_clock_feed`]'s
    /// subscriber -> handle driving, without going through the full
    /// `run_with`/`ClockMode::Simulation` runner path (that full path, plus the
    /// actual wire-key match with the simulation controller's publisher, is
    /// covered by `tests/runner.rs`'s
    /// `simulation_mode_step_advances_only_with_the_clock_feed`).
    ///
    /// Publishes synthetic `simulation::Clock` samples through a real
    /// `TimelineAuthority` onto an in-process bus (standing in for the
    /// controller) and asserts: the scheduler releases a tick per sample, on
    /// the published timeline (a replacement timeline is observed even though
    /// its ticks restart at 0), and no further tick releases while no sample is
    /// published.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simulation_clock_feed_drives_the_scheduler_from_published_samples() {
        let bus_config = BusConfig::in_process("test/sim-clock-feed-unit", "robot");
        let bus = Bus::open(bus_config).await.expect("bus should open");

        let clock_publisher = StatePublisher::<api::simulation::Clock>::new(
            bus.clone(),
            &api::topic::owner().simulation().clock(),
        )
        .expect("clock publisher should attach");
        let mut authority =
            TimelineAuthority::__mint(line(11)).expect("the world authority should mint");

        let period = Duration::from_millis(10);
        // Generous hang-guard for the positive release waits below. A correct
        // feed releases the tick near-instantly once the sample arrives, so
        // this deadline only trips on a genuine hang; it is sized to tolerate a
        // starved runner (e.g. emulated musl under CI), not to assert latency.
        let release_guard = Duration::from_secs(10);
        let (scheduler, handle) =
            SimulationScheduler::new(crate::participant::spec::MissedTick::Collapse, Some(period));
        let feed_task = spawn_simulation_clock_feed(&bus, handle).expect("feed task should spawn");

        // No sample published yet: the scheduler must not release the first tick.
        let period_ns = duration_to_nanos_saturating(period);
        let first_target = at(11, period_ns);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                scheduler.wait_until(first_target),
            )
            .await
            .is_err(),
            "scheduler must not release before any simulation/clock sample arrives"
        );

        // Publish an advancing sample; the pending wait should now resolve.
        clock_publisher
            .publish(
                &authority.completed_step(period_ns),
                api::simulation::Clock { step: 1 },
            )
            .expect("clock sample should publish");
        let tick = tokio::time::timeout(release_guard, scheduler.wait_until(first_target))
            .await
            .expect("scheduler should release once the feed advances past the target");
        assert_eq!(tick.fired_at, first_target);

        // No new sample means no world advance and therefore no scheduler
        // release.
        let second_target = at(11, 2 * period_ns);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(200),
                scheduler.wait_until(second_target),
            )
            .await
            .is_err(),
            "the scheduler must remain still while no new clock sample arrives"
        );

        // The next published world step releases the withheld tick.
        clock_publisher
            .publish(
                &authority.completed_step(2 * period_ns),
                api::simulation::Clock { step: 2 },
            )
            .expect("second clock sample should publish");
        let tick = tokio::time::timeout(release_guard, scheduler.wait_until(second_target))
            .await
            .expect("scheduler should release on the next clock sample");
        assert_eq!(tick.fired_at, second_target);

        // A replacement world history is accepted once. A later in-flight clock
        // from the retired controller must not reactivate it.
        authority.replace_timeline(line(12));
        clock_publisher
            .publish(
                &authority.completed_step(0),
                api::simulation::Clock { step: 0 },
            )
            .expect("replacement clock should publish");
        let tick = tokio::time::timeout(release_guard, scheduler.wait_until(at(11, 3 * period_ns)))
            .await
            .expect("replacement timeline should reach the scheduler");
        assert_eq!(tick.fired_at, at(12, 0));

        authority.replace_timeline(line(11));
        clock_publisher
            .publish(
                &authority.completed_step(3 * period_ns),
                api::simulation::Clock { step: 3 },
            )
            .expect("late retired clock should publish at the bus layer");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            scheduler.now(),
            Some(at(12, 0)),
            "a retired clock must not roll the scheduler back to an old world history"
        );

        feed_task.abort();
        bus.close().await.expect("bus should close");
    }
}
