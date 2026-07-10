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
//! fresh-envelope publishes (`Latest::latest()` is an `ArcSwapOption` load,
//! `Publisher`/`Querier` build a new envelope per call, `Server` carries no
//! live connection), so `api` and every `api_shared` clone always observe
//! and produce the identical live state - they can never diverge. That is
//! D3's "an api snapshot", realized without a lock, a `RwLock`, or `unsafe`.
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

use crate::bus::{LogicalTime, QueryFailure, Subscriber};
use crate::participant::api::ParticipantLifecycle;
use crate::participant::bus_log::{self, BusLogState};
use crate::participant::clock::{ClockSource, RealClock};
use crate::participant::context::{SetupContext, ShutdownContext, StepContext};
use crate::participant::heartbeat::{self, HeartbeatPublisher};
use crate::participant::launch::{ClockMode, ParticipantLaunch};
use crate::participant::managed::{ManagedTaskExit, ManagedTasks};
use crate::participant::scheduler::{
    AnyStepScheduler, RealScheduler, SchedulerTick, SimulationClockHandle, SimulationScheduler,
    StepScheduler, duration_to_nanos_saturating,
};
use crate::participant::spec::StepSchedule;
use phoxal_api::y2026_1 as api;
use phoxal_bus::{Bus, BusConfig, IncomingQuery};

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// The default binary entrypoint:
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
    let launch = ParticipantLaunch::from_cli(R::ID, "robot")?;

    init_tracing();

    let clock = launch_clock(&launch)?;
    run_with::<R, _, _>(launch, clock, shutdown_signal()).await
}

/// The timestamp source for a launch. Both clock modes stamp
/// `StepContext`/`produced_at_ns` from the same host-wide real clock (D34) -
/// what [`ClockMode::Simulation`] changes is *when* `#[step]` ticks are
/// released (see [`step_scheduler_for`] and [`spawn_simulation_clock_feed`],
/// which now drives that release from the live `simulation/clock` bus feed),
/// not what time is read once a tick fires. A `ClockSource` that instead
/// subscribes the authoritative `simulation/clock` for timestamps is future
/// work (the Webots port, see the `clock` module docs); until it lands,
/// simulation mode still timestamps from the host-monotonic domain.
pub(crate) fn launch_clock(launch: &ParticipantLaunch) -> crate::Result<RealClock> {
    match launch.clock {
        ClockMode::Real | ClockMode::Simulation => Ok(RealClock::new()),
    }
}

/// Select the step scheduler for `clock_mode` (D34/#09): the seam that
/// answers "when should the next `#[step]` tick fire", separate from the
/// [`ClockSource`] used for timestamps (see [`launch_clock`]).
///
/// [`ClockMode::Real`] preserves the runner's pre-#09 wall-clock cadence
/// exactly, returning no driving handle (`None`). [`ClockMode::Simulation`]
/// starts a [`SimulationScheduler`] and returns its [`SimulationClockHandle`]
/// (`Some`) so the caller can wire the live `simulation/clock` subscription
/// (see [`spawn_simulation_clock_feed`]): the handle is the attachment point
/// anything that produces a `LogicalTime` drives the scheduler through (a bus
/// subscription task, a test, a REPL).
pub(crate) fn step_scheduler_for(
    clock_mode: ClockMode,
    schedule: Option<StepSchedule>,
    now: LogicalTime,
) -> (AnyStepScheduler, Option<SimulationClockHandle>) {
    let missed_tick = schedule
        .map(|s| s.missed_tick)
        .unwrap_or(crate::participant::spec::MissedTick::Collapse);
    let period = schedule.map(|s| s.period());
    match clock_mode {
        ClockMode::Real => (
            AnyStepScheduler::Real(RealScheduler::new(missed_tick, period, now)),
            None,
        ),
        ClockMode::Simulation => {
            let (scheduler, handle) = SimulationScheduler::new(missed_tick, period, now);
            (AnyStepScheduler::Simulation(scheduler), Some(handle))
        }
    }
}

/// Subscribe the authoritative `simulation/clock` feed (published by the
/// `Simulator` kind that owns the world, e.g. the Webots supervisor) and drive
/// `handle` from it for the lifetime of the returned task.
///
/// Mirrors the snapshot-server task pattern (bus-driven task, pushed alongside
/// the other server tasks, aborted at shutdown): this subscribes the same
/// global `simulation/clock` wire key every sim participant on the robot
/// observes (`api::topic::new().simulation().clock()`, the CLIENT side of the
/// `Simulator`'s owner-side publish - both sides format the identical
/// `simulation/clock` key, D61/D62), then per received sample:
///
/// - calls [`SimulationClockHandle::advance`] with the sample's ENVELOPE
///   logical time (`metadata.epoch` + `metadata.produced_at_ns`), not just the
///   body's `now_ns` - the envelope carries the supervisor's epoch, so a reset
///   (epoch bump, `now_ns` back to 0) is conveyed correctly: `LogicalTime`'s
///   derived `Ord` is lexicographic on `(epoch, time_ns)`, so a bumped epoch
///   always advances even though `time_ns` drops back to 0.
/// - pauses the scheduler on `running == false`, resumes it on
///   `running == true` (idempotent either way).
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
        let topic = api::topic::new().simulation().clock();
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
        while let Ok(received) = subscriber.recv().await {
            let at = LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns);
            handle.advance(at);
            if received.body.running {
                handle.resume();
            } else {
                handle.pause();
            }
        }
    }))
}

/// Run a participant against an explicit launch, clock, and shutdown trigger. The
/// seam the test harness + integration tests drive (D41).
pub async fn run_with<R, C, S>(
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

    let bus = Bus::open(BusConfig {
        namespace: launch.namespace.clone(),
        robot_id: launch.robot_id.clone(),
        participant: launch.participant_id.clone(),
        incarnation: 0,
        connect_endpoints: launch.bus.connect_endpoints.clone(),
    })
    .await?;

    let result = run_with_bus::<R, C, S>(&bus, launch, clock, shutdown).await;

    if let Err(e) = bus.close().await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "bus close failed");
    }
    result
}

/// Run a participant on a **caller-owned** bus, against an explicit launch, clock, and
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
pub async fn run_with_bus<R, C, S>(
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
    let mut heartbeat = HeartbeatPublisher::attach(bus.clone(), launch.participant_id.clone());
    heartbeat.publish(clock.now());

    let result =
        run_lifecycle_inner::<R, C, S>(bus, launch, &clock, shutdown, &mut heartbeat).await;
    if result.is_err() {
        heartbeat.set_readiness(api::presence::Readiness::Failed);
        heartbeat.publish(clock.now());
    }
    result
}

async fn run_lifecycle_inner<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: &C,
    shutdown: S,
    heartbeat: &mut HeartbeatPublisher,
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

    // Mint the single plan #00 Layer 2 owner capability the participant uses to opt
    // into owning its own topics (via `ctx.owner_capability()` ->
    // `api::topic::internal::new(cap)`). The runner is the only minter.
    let mut ctx = SetupContext::<R>::new(
        bus.clone(),
        ::phoxal_bus::OwnerCap::__mint(),
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
        let queryable = bus.declare_server(topic).await?;
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
        let queryable = bus.declare_server(topic).await?;
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

    let schedule = R::__step_schedule();
    let (scheduler, clock_handle) = step_scheduler_for(launch.clock, schedule, clock.now());
    // `ClockMode::Simulation` hands back a driving handle: keep it alive by
    // moving it into the feed task rather than dropping it (the
    // `SimulationScheduler` also retains its own sender keepalive, so the
    // watch channel would not actually close either way - see the scheduler's
    // keepalive docs - but the handle is only useful for as long as something
    // holds it and drives it, which is this task's whole job). Pushed
    // alongside the other bus-driven tasks so it is aborted at shutdown.
    if let Some(handle) = clock_handle {
        server_tasks.push(spawn_simulation_clock_feed(bus, handle)?);
    }
    let shutdown = pin!(shutdown);
    heartbeat.set_readiness(api::presence::Readiness::Ready);
    heartbeat.publish(clock.now());
    tracing::info!(target: "phoxal.runtime", id = R::ID, participant = %launch.participant_id, "runtime ready");
    super::sd_notify::ready();
    let watchdog = super::sd_notify::Watchdog::start();
    let fault = main_loop::<R, C, S>(
        &mut participant,
        &mut api,
        bus,
        clock,
        &scheduler,
        schedule,
        &committed,
        &mut excl_rx,
        shutdown,
        heartbeat,
        &watchdog,
        &mut managed_tasks,
    )
    .await;
    watchdog.shutdown();
    heartbeat.set_readiness(api::presence::Readiness::Degraded);
    heartbeat.publish(clock.now());
    drop(excl_tx);

    for task in server_tasks {
        task.abort();
    }

    let grace = Duration::from_millis(launch.shutdown_grace_ms);
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
        participant.__shutdown(&mut api, ShutdownContext::new(grace)),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(target: "phoxal.runtime", error = %e, "shutdown hook returned error");
        }
        Err(_elapsed) => {
            tracing::warn!(
                target: "phoxal.runtime",
                grace_ms = launch.shutdown_grace_ms,
                "shutdown hook exceeded the grace deadline; proceeding to bus close"
            );
        }
    }

    // Join managed tasks before the bus closes (item 4/5/6 of the managed-task
    // contract): the same shutdown deadline bounds both `#[shutdown]` and
    // managed-task joining, so a stuck task cannot extend the grace window.
    let unjoined = managed_tasks.join_until(shutdown_deadline).await;
    log_unjoined_managed_tasks(unjoined, launch.shutdown_grace_ms);

    tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");

    if let Some(fault) = fault {
        return Err(managed_task_fault_error(&fault));
    }
    Ok(())
}

/// Build the runtime-fault error for an unexpected `FaultOnExit` managed task
/// exit. Returned from `run_lifecycle_inner` so it flows through the same
/// `Result` path `run_lifecycle` already turns into `Readiness::Failed`.
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
    heartbeat: &mut HeartbeatPublisher,
    watchdog: &super::sd_notify::Watchdog,
    managed_tasks: &mut ManagedTasks,
) -> Option<ManagedTaskExit>
where
    R: ParticipantLifecycle,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let period = schedule.map(|s| s.period());
    let mut step_index: u64 = 0;
    let mut last_time_ns = clock.now().time_ns();
    // The next tick's *logical* due time - what the runner asks the scheduler
    // to release at (D34/#09), separate from the wall-clock heartbeat below.
    let mut next_step_target = period.map(|period| {
        let now = scheduler.now();
        advance_logical_deadline(now, period, 0)
    });
    let mut next_heartbeat = tokio::time::Instant::now();

    loop {
        tokio::select! {
            // Order matters: shutdown first, then a managed-task fault (both are
            // "stop the loop" events and should preempt routine work), then the
            // framework health tick, then a *due* step, then server queries.
            // Health is cheap and must not be starved by an overloaded
            // participant; due steps still take priority over a steady query
            // backlog. `Some(..)` disables the query branch if the channel ever
            // closes, so it never busy-loops.
            biased;
            _ = &mut shutdown => return None,
            exit = managed_tasks.next_unexpected_exit() => {
                tracing::error!(
                    target: "phoxal.runtime",
                    task = %exit.name,
                    panic = exit.panic_message.as_deref(),
                    "managed task exited unexpectedly; faulting the participant"
                );
                return Some(exit);
            }
            _ = heartbeat_tick(next_heartbeat) => {
                heartbeat.publish(clock.now());
                watchdog.feed();
                advance_deadline(&mut next_heartbeat, heartbeat::HEARTBEAT_INTERVAL);
            }
            SchedulerTick { missed_ticks, .. } = step_tick(scheduler, next_step_target) => {
                let (Some(period), Some(target)) = (period, next_step_target) else { continue };
                next_step_target = Some(advance_logical_deadline(target, period, missed_ticks));

                let now = clock.now();
                let dt_ns = now.time_ns().saturating_sub(last_time_ns);
                last_time_ns = now.time_ns();

                let step = StepContext::new(now.epoch(), step_index, now.time_ns(), dt_ns, missed_ticks);
                step_index += 1;

                // A handler `Err` is a domain outcome: stay healthy, log, continue
                // (D32); the snapshot is committed only after a *successful* step so
                // a failed mutation is never published as committed state. A panic
                // would unwind and abort the process.
                match participant.__step(api, step).await {
                    Ok(()) => commit_snapshot::<R>(participant, committed),
                    Err(e) => {
                        tracing::warn!(target: "phoxal.runtime", error = %e, "step returned error");
                    }
                }
                watchdog.feed();
            }
            Some(incoming) = excl_rx.recv() => {
                // Commit only if the handler succeeded (D14/D32: retain the prior
                // snapshot on a handler error).
                if serve_exclusive_query::<R>(participant, api, bus, incoming).await {
                    commit_snapshot::<R>(participant, committed);
                }
                watchdog.feed();
            }
        }
    }
}

pub(crate) fn advance_logical_deadline(
    target: LogicalTime,
    period: Duration,
    missed_ticks: u32,
) -> LogicalTime {
    let period_ns = duration_to_nanos_saturating(period);
    let periods = u64::from(missed_ticks).saturating_add(1);
    LogicalTime::new(
        target.epoch(),
        target
            .time_ns()
            .saturating_add(period_ns.saturating_mul(periods)),
    )
}

pub(crate) async fn heartbeat_tick(next: tokio::time::Instant) {
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
    target: Option<LogicalTime>,
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

    #[test]
    fn simulation_clock_launch_is_accepted() {
        // #09: PHOXAL_CLOCK=simulation is no longer rejected - the runner
        // selects a `SimulationScheduler` for it (see `step_scheduler_for`)
        // instead of bailing before the bus even opens.
        let mut launch = ParticipantLaunch::local("participant", "robot");
        launch.clock = ClockMode::Simulation;

        launch_clock(&launch).expect("simulation clock launch should be accepted");
    }

    #[test]
    fn step_scheduler_for_selects_real_or_simulation_by_clock_mode() {
        let now = LogicalTime::new(0, 0);
        let schedule = Some(StepSchedule::hz(100.0));

        let (real, real_handle) = step_scheduler_for(ClockMode::Real, schedule, now);
        assert!(matches!(real, AnyStepScheduler::Real(_)));
        assert!(
            real_handle.is_none(),
            "real mode has no simulation clock handle to drive"
        );

        let (simulation, simulation_handle) =
            step_scheduler_for(ClockMode::Simulation, schedule, now);
        assert!(matches!(simulation, AnyStepScheduler::Simulation(_)));
        assert!(
            simulation_handle.is_some(),
            "simulation mode must hand back the driving handle so the caller can wire the live feed"
        );
    }

    #[test]
    fn logical_step_deadline_skips_collapsed_ticks() {
        let next = advance_logical_deadline(LogicalTime::new(0, 10), Duration::from_nanos(10), 3);

        assert_eq!(
            next,
            LogicalTime::new(0, 50),
            "target 10 plus the fired period and 3 collapsed periods should resume at 50"
        );
    }

    #[test]
    fn logical_step_deadline_saturates_instead_of_wrapping() {
        let next = advance_logical_deadline(
            LogicalTime::new(2, u64::MAX - 2),
            Duration::from_nanos(10),
            3,
        );

        assert_eq!(next, LogicalTime::new(2, u64::MAX));
    }

    #[tokio::test]
    async fn simulation_scheduler_selected_by_the_runner_schedules_deterministically() {
        // Exercises the exact scheduler + handle `step_scheduler_for` selects
        // for `ClockMode::Simulation`, driven purely by logical time via its
        // own `SimulationClockHandle` - no real sleeping, no live bus/Webots
        // feed. This is the deterministic proof that simulation mode
        // schedules ticks from logical time (acceptance criterion); the full
        // `run_with`/`ClockMode::Simulation` integration path (live feed
        // wiring included) is covered by `tests/runner.rs`.
        let schedule = StepSchedule::hz(10.0); // 100ms period
        let period_ns = duration_to_nanos_saturating(schedule.period());
        let start = LogicalTime::new(0, 0);

        let (scheduler, handle) = step_scheduler_for(ClockMode::Simulation, Some(schedule), start);
        let handle = handle.expect("simulation mode must hand back a driving handle");

        let mut fired = Vec::new();
        let mut target = LogicalTime::new(0, period_ns);
        for _ in 0..3 {
            handle.advance(target);
            let tick = scheduler.wait_until(target).await;
            fired.push(tick.fired_at.time_ns());
            target = LogicalTime::new(0, target.time_ns() + period_ns);
        }

        assert_eq!(
            fired,
            vec![100_000_000, 200_000_000, 300_000_000],
            "ticks fire in order at the logical times the handle advanced to, with no real sleeping"
        );
    }

    /// Isolation-level proof of [`spawn_simulation_clock_feed`]'s
    /// subscriber -> handle driving, without going through the full
    /// `run_with`/`ClockMode::Simulation` runner path (that full path, plus
    /// the actual wire-key match with the Webots supervisor's publisher, is
    /// covered by `tests/runner.rs`'s
    /// `simulation_mode_step_advances_only_with_the_clock_feed`).
    ///
    /// Publishes synthetic `simulation::Clock` samples directly onto an
    /// in-process bus (standing in for the Webots supervisor) and asserts:
    /// the scheduler releases a tick per advance, in the published epoch's
    /// domain (a reset - epoch bump - is observed even though `now_ns` drops
    /// back to 0), and a `running = false` sample withholds release until a
    /// later `running = true` sample resumes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simulation_clock_feed_drives_the_scheduler_from_published_samples() {
        let bus_config = BusConfig::in_process("test/sim-clock-feed-unit", "robot");
        let bus = Bus::open(bus_config).await.expect("bus should open");

        let clock_publisher = crate::bus::Publisher::<api::simulation::Clock>::new(
            bus.clone(),
            &api::topic::internal::new(crate::bus::OwnerCap::__mint())
                .simulation()
                .clock(),
        )
        .expect("clock publisher should attach");

        let period = Duration::from_millis(10);
        // Generous hang-guard for the positive release waits below. A correct
        // feed releases the tick near-instantly once the sample arrives, so
        // this deadline only trips on a genuine hang; it is sized to tolerate a
        // starved runner (e.g. emulated musl under CI), not to assert latency.
        let release_guard = Duration::from_secs(10);
        let (scheduler, handle) = SimulationScheduler::new(
            crate::participant::spec::MissedTick::Collapse,
            Some(period),
            LogicalTime::new(0, 0),
        );
        let feed_task = spawn_simulation_clock_feed(&bus, handle).expect("feed task should spawn");

        // No sample published yet: the scheduler must not release the first tick.
        let period_ns = duration_to_nanos_saturating(period);
        let first_target = LogicalTime::new(0, period_ns);
        let pending = tokio::time::timeout(
            Duration::from_millis(100),
            scheduler.wait_until(first_target),
        )
        .await;
        assert!(
            pending.is_err(),
            "scheduler must not release before any simulation/clock sample arrives"
        );

        // Publish an advancing sample; the pending wait should now resolve.
        clock_publisher
            .publish_at(
                first_target,
                api::simulation::Clock {
                    now_ns: period_ns,
                    running: true,
                },
            )
            .await
            .expect("clock sample should publish");
        let tick = tokio::time::timeout(release_guard, scheduler.wait_until(first_target))
            .await
            .expect("scheduler should release once the feed advances past the target");
        assert_eq!(tick.fired_at, first_target);

        // Publish a paused sample whose envelope logical time is past the next
        // target: release must still be withheld.
        let second_target = LogicalTime::new(0, 2 * period_ns);
        clock_publisher
            .publish_at(
                second_target,
                api::simulation::Clock {
                    now_ns: 2 * period_ns,
                    running: false,
                },
            )
            .await
            .expect("paused clock sample should publish");
        let still_pending = tokio::time::timeout(
            Duration::from_millis(200),
            scheduler.wait_until(second_target),
        )
        .await;
        assert!(
            still_pending.is_err(),
            "running=false must withhold release even though logical time already reached the target"
        );

        // Resume (still at the same logical time, `running` flips back true):
        // the withheld tick should now release.
        clock_publisher
            .publish_at(
                second_target,
                api::simulation::Clock {
                    now_ns: 2 * period_ns,
                    running: true,
                },
            )
            .await
            .expect("resume clock sample should publish");
        let tick = tokio::time::timeout(release_guard, scheduler.wait_until(second_target))
            .await
            .expect("scheduler should release once resumed");
        assert_eq!(tick.fired_at, second_target);

        feed_task.abort();
        bus.close().await.expect("bus should close");
    }
}
