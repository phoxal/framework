//! The NEW-model runner (F-runtime slice): the [`participant::api`](super::api)
//! trait hierarchy's counterpart to [`runner`](super::runner), coexisting with
//! it unmodified (see that module's docs for the OLD model it drives).
//!
//! Mirrors [`runner`](super::runner)'s `run`/`run_async`/`run_with`/
//! `run_with_bus`/`run_lifecycle`/`run_lifecycle_inner`/`main_loop` structure
//! exactly - same bus-open/heartbeat/server-declare/scheduler/shutdown-grace
//! sequencing - so this module reuses every R-independent helper from
//! [`runner`](super::runner) (`launch_clock`, `step_scheduler_for`,
//! `spawn_simulation_clock_feed`, the deadline/tick helpers,
//! `managed_task_fault_error`, `log_unjoined_managed_tasks`, `init_tracing`,
//! `shutdown_signal`) rather than duplicating them; only the R-bound
//! lifecycle-dispatch functions (`commit_snapshot`, `serve_exclusive_query`,
//! `serve_snapshot_query`, `main_loop`, `run_lifecycle*`) are rewritten here
//! against [`ParticipantLifecycle`] instead of
//! [`ParticipantBehavior`](super::spec::ParticipantBehavior), because their
//! signatures thread `Self::Api` through in a way the OLD versions cannot
//! (D3/RECONCILIATION correction #6).
//!
//! # `Api` ownership (D3: "read-only `&Self::Api`, or an api snapshot")
//!
//! `#[setup]` returns `(participant, api)` as two independent values
//! (`ParticipantLifecycle::__setup`). This runner keeps:
//!
//! - **`api: R::Api`**, owned directly (not behind `Arc`) - passed as
//!   `&mut Self::Api` to `#[step]`/exclusive `#[server]`/`#[shutdown]`, all
//!   awaited serially on the main task (same exclusivity rule as the OLD
//!   model's `#[step]`/`#[server]`, D16), so a plain owned value always gives
//!   a sound `&mut` with no synchronization needed;
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
//! anti-pattern - see `Subscriber`'s and `Subscriber::recv`'s rustdoc). No
//! code in this slice violates that; P-convert authors must uphold it per
//! participant. **Deferred guard:** this rule is documentation-only for now -
//! a compile-time reject of a `#[server_snapshot]` handler that `recv`s a
//! `Subscriber` field would need the snapshot codegen to see the `Api` field
//! kinds (which it does not today), so it is left as a hardening follow-up
//! rather than an enforced invariant in this slice.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::bus::QueryFailure;
use crate::participant::api::ParticipantLifecycle;
use crate::participant::bus_log;
use crate::participant::clock::ClockSource;
use crate::participant::context::{SetupContext, ShutdownContext, StepContext};
use crate::participant::heartbeat::HeartbeatPublisher;
use crate::participant::launch::{LaunchAction, ParticipantLaunch};
use crate::participant::managed::{ManagedTaskExit, ManagedTasks};
use crate::participant::runner::{
    advance_deadline, advance_logical_deadline, heartbeat_tick, init_tracing, launch_clock,
    log_unjoined_managed_tasks, managed_task_fault_error, shutdown_signal,
    spawn_simulation_clock_feed, step_scheduler_for, step_tick,
};
use crate::participant::scheduler::{AnyStepScheduler, SchedulerTick, StepScheduler};
use crate::participant::spec::StepSchedule;
use phoxal_api::y2026_1 as api;
use phoxal_bus::{Bus, BusConfig, IncomingQuery};

/// Run a NEW-model participant to completion on a framework-owned blocking
/// Tokio runtime. The `#[phoxal::service|driver|simulator|tool]` +
/// `#[phoxal::behavior]` counterpart to [`run`](super::runner::run) (OLD
/// model, `#[derive(phoxal::Service|...)]`).
///
/// The default binary entrypoint for a new-model participant:
/// `fn main() -> phoxal::Result<()> { phoxal::run_v2::<Participant>() }`.
pub fn run_v2<R: ParticipantLifecycle>() -> crate::Result<()> {
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tokio_runtime.block_on(run_async_v2::<R>())
}

/// Async host runner for custom Tokio mains
/// (`phoxal::tokio::run_v2::<Participant>().await`).
pub async fn run_async_v2<R: ParticipantLifecycle>() -> crate::Result<()> {
    let launch = match ParticipantLaunch::from_cli(R::ID, "robot")? {
        LaunchAction::Run(launch) => launch,
        // The OLD model's `emit-apis` subcommand prints runner-executed
        // metadata (`super::emit::print_emit_apis`, `ParticipantBehavior`-bound
        // - not reachable here). A new-model participant's contract/config
        // metadata is embedded at compile time in a `#[link_section]` static
        // by `#[derive(phoxal::Api)]`/`#[derive(phoxal::Config)]`
        // (`participant::api`'s module docs; RECONCILIATION correction #12 /
        // "Removing emit-apis"): there is nothing to execute to learn it, so
        // this is a documented no-op rather than a silent one.
        LaunchAction::EmitApis => {
            eprintln!(
                "note: '{}' is a new-model participant (#[phoxal::service|driver|simulator|tool]); \
                 its metadata is embedded at compile time in the built artifact, not printed at \
                 runtime - `emit-apis` is a no-op for it",
                R::ID
            );
            return Ok(());
        }
    };

    init_tracing();

    let clock = launch_clock(&launch)?;
    run_with_v2::<R, _, _>(launch, clock, shutdown_signal()).await
}

/// Run a NEW-model participant against an explicit launch, clock, and
/// shutdown trigger - the test-harness seam, mirroring
/// [`run_with`](super::runner::run_with).
pub async fn run_with_v2<R, C, S>(
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

    let result = run_with_bus_v2::<R, C, S>(&bus, launch, clock, shutdown).await;

    if let Err(e) = bus.close().await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "bus close failed");
    }
    result
}

/// Run a NEW-model participant on a **caller-owned** bus - the embedding seam
/// for co-locating participants on one in-process [`Bus`], mirroring
/// [`run_with_bus`](super::runner::run_with_bus). See that function's docs for
/// the shared-bus `source` identity caveat, which applies identically here.
pub async fn run_with_bus_v2<R, C, S>(
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
    let result = run_lifecycle_v2::<R, C, S>(bus, launch, clock, shutdown).await;
    bus_logs.shutdown().await;
    result
}

async fn run_lifecycle_v2<R, C, S>(
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
        run_lifecycle_inner_v2::<R, C, S>(bus, launch, &clock, shutdown, &mut heartbeat).await;
    if result.is_err() {
        heartbeat.set_readiness(api::presence::Readiness::Failed);
        heartbeat.publish(clock.now());
    }
    result
}

async fn run_lifecycle_inner_v2<R, C, S>(
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

    let robot = match &launch.robot_root {
        Some(root) => Some(Arc::new(crate::model::v0::Robot::read_from_dir(root)?)),
        None => None,
    };

    // Mint the single plan #00 Layer 2 owner capability, exactly as the OLD
    // runner does (`SetupContext::new` is unbounded on `R` - see that
    // function's docs for why it moved there in this slice).
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
    let mut managed_tasks = ctx.take_managed_tasks();

    // The Api ownership split (see module docs): `api` stays owned for the
    // exclusive `&mut Self::Api` path; `api_shared` is the one clone every
    // concurrent `#[server_snapshot]` task gets its own `Arc::clone` of.
    let api_shared: Arc<R::Api> = Arc::new(api.clone());
    let mut api = api;

    let committed: Arc<ArcSwapOption<R::Snapshot>> = Arc::new(ArcSwapOption::empty());
    commit_snapshot::<R>(&participant, &committed);

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
                    Some(_) = inflight.join_next() => {}
                }
            }
        }));
    }

    let schedule = R::__step_schedule();
    let (scheduler, clock_handle) = step_scheduler_for(launch.clock, schedule, clock.now());
    if let Some(handle) = clock_handle {
        server_tasks.push(spawn_simulation_clock_feed(bus, handle)?);
    }
    let shutdown = pin!(shutdown);
    heartbeat.set_readiness(api::presence::Readiness::Ready);
    heartbeat.publish(clock.now());
    tracing::info!(target: "phoxal.runtime", id = R::ID, participant = %launch.participant_id, "runtime ready (v2)");
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

    let unjoined = managed_tasks.join_until(shutdown_deadline).await;
    log_unjoined_managed_tasks(unjoined, launch.shutdown_grace_ms);

    tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped (v2)");

    if let Some(fault) = fault {
        return Err(managed_task_fault_error(&fault));
    }
    Ok(())
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
    let mut next_step_target = period.map(|period| {
        let now = scheduler.now();
        advance_logical_deadline(now, period, 0)
    });
    let mut next_heartbeat = tokio::time::Instant::now();

    loop {
        tokio::select! {
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
                advance_deadline(&mut next_heartbeat, crate::participant::heartbeat::HEARTBEAT_INTERVAL);
            }
            SchedulerTick { missed_ticks, .. } = step_tick(scheduler, next_step_target) => {
                let (Some(period), Some(target)) = (period, next_step_target) else { continue };
                next_step_target = Some(advance_logical_deadline(target, period, missed_ticks));

                let now = clock.now();
                let dt_ns = now.time_ns().saturating_sub(last_time_ns);
                last_time_ns = now.time_ns();

                let step = StepContext::new(now.epoch(), step_index, now.time_ns(), dt_ns, missed_ticks);
                step_index += 1;

                match participant.__step(api, step).await {
                    Ok(()) => commit_snapshot::<R>(participant, committed),
                    Err(e) => {
                        tracing::warn!(target: "phoxal.runtime", error = %e, "step returned error");
                    }
                }
                watchdog.feed();
            }
            Some(incoming) = excl_rx.recv() => {
                if serve_exclusive_query::<R>(participant, api, bus, incoming).await {
                    commit_snapshot::<R>(participant, committed);
                }
                watchdog.feed();
            }
        }
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
/// runner should commit a fresh snapshot). Mirrors
/// [`serve_exclusive_query`](super::runner) (OLD model), threading `&mut
/// R::Api` alongside `&mut R` (D3).
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

/// Serve one concurrent `#[server_snapshot]` query, mirroring
/// [`serve_snapshot_query`](super::runner) (OLD model) but also handing the
/// generated dispatcher its `Arc<R::Api>` clone (D3).
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
