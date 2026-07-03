//! The runner (D23/D34): owns the bus connection, the clock, step scheduling,
//! server-query dispatch, snapshot commits, and graceful shutdown.
//!
//! `phoxal::run::<R>()` builds a blocking Tokio runtime and runs the participant to
//! completion; `phoxal::tokio::run::<R>().await` is the async entrypoint for
//! custom Tokio mains.
//!
//! Serving model (D16): exclusive `#[server]` queries are awaited on the main
//! task (holding `&mut self`, serialized with `#[step]`); concurrent
//! `#[server_snapshot]` queries are spawned and read a committed `Snapshot`. A
//! snapshot is committed after `#[setup]`, after each `#[step]`, and after each
//! exclusive `#[server]`.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::bus::QueryFailure;
use crate::participant::bus_log::{self, BusLogState};
use crate::participant::clock::{ClockSource, RealClock};
use crate::participant::context::{SetupContext, ShutdownContext, StepContext};
use crate::participant::emit::print_emit_apis;
use crate::participant::launch::{ClockMode, LaunchAction, ParticipantLaunch};
use crate::participant::spec::{MissedTick, ParticipantBehavior, StepSchedule};
use phoxal_bus::{Bus, BusConfig, IncomingQuery};

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// The default binary entrypoint:
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
pub fn run<R: ParticipantBehavior>() -> crate::Result<()> {
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tokio_runtime.block_on(run_async::<R>())
}

/// Async host runner for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
pub async fn run_async<R: ParticipantBehavior>() -> crate::Result<()> {
    let launch = match ParticipantLaunch::from_cli(R::ID, "robot")? {
        LaunchAction::Run(launch) => launch,
        // The `emit-apis` subcommand short-circuits before config / tracing /
        // Zenoh / setup. The compiled-in metadata is authoritative (D50).
        LaunchAction::EmitApis => {
            print_emit_apis::<R>();
            return Ok(());
        }
    };

    init_tracing();

    let clock = launch_clock(&launch)?;
    run_with::<R, _, _>(launch, clock, shutdown_signal()).await
}

fn launch_clock(launch: &ParticipantLaunch) -> crate::Result<RealClock> {
    match launch.clock {
        ClockMode::Real => Ok(RealClock::new()),
        ClockMode::Simulation => anyhow::bail!(
            "PHOXAL_CLOCK=simulation requested, but the simulation clock is not yet supported"
        ),
    }
}

/// Run a participant against an explicit launch, clock, and shutdown trigger. The
/// seam the test harness + integration tests drive (D41).
pub async fn run_with<R, C, S>(
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantBehavior,
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
/// `launch` here drives config, bundle/model, and component-instance resolution.
pub async fn run_with_bus<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: ParticipantBehavior,
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
    R: ParticipantBehavior,
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
        Some(root) => Some(Arc::new(crate::model::v1::Robot::read_from_dir(root)?)),
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
    let mut participant = R::__setup(&mut ctx, config).await?;

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
        let bus = bus.clone();
        server_tasks.push(tokio::spawn(async move {
            let mut inflight = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = queryable.recv() => {
                        let Ok(incoming) = incoming else { break };
                        let snapshot = committed.load_full();
                        let bus = bus.clone();
                        inflight.spawn(async move {
                            serve_snapshot_query::<R>(&bus, incoming, snapshot).await
                        });
                    }
                    // Reap finished handlers so the JoinSet does not grow unbounded.
                    Some(_) = inflight.join_next() => {}
                }
            }
        }));
    }

    let schedule = R::__step_schedule();
    let shutdown = pin!(shutdown);
    tracing::info!(target: "phoxal.runtime", id = R::ID, participant = %launch.participant_id, "runtime ready");
    super::sd_notify::ready();
    main_loop::<R, C, S>(
        &mut participant,
        bus,
        &clock,
        schedule,
        &committed,
        &mut excl_rx,
        shutdown,
    )
    .await;
    drop(excl_tx);

    for task in server_tasks {
        task.abort();
    }

    // Bound the shutdown hook by the grace deadline (D24/D43i): a hook that
    // parks/flushes hardware can hang, but the runner must still proceed to
    // bus close deterministically rather than leak the process. On timeout we
    // log and move on; the hook's task is dropped (cancelled at the next await).
    let grace = Duration::from_millis(launch.shutdown_grace_ms);
    match tokio::time::timeout(grace, participant.__shutdown(ShutdownContext::new(grace))).await {
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
    tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn main_loop<R, C, S>(
    participant: &mut R,
    bus: &Bus,
    clock: &C,
    schedule: Option<StepSchedule>,
    committed: &Arc<ArcSwapOption<R::Snapshot>>,
    excl_rx: &mut mpsc::Receiver<IncomingQuery>,
    mut shutdown: std::pin::Pin<&mut S>,
) where
    R: ParticipantBehavior,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let period = schedule.map(|s| s.period());
    let mut step_index: u64 = 0;
    let mut last_time_ns = clock.now().time_ns();
    let mut next = tokio::time::Instant::now() + period.unwrap_or_else(|| Duration::from_secs(1));

    loop {
        tokio::select! {
            // Order matters: shutdown first, then a *due* step, then server
            // queries. A due step takes priority so a steady query backlog cannot
            // starve the control loop; between steps (timer pending) queries are
            // served. `Some(..)` disables the query branch if the channel ever
            // closes, so it never busy-loops.
            biased;
            _ = &mut shutdown => return,
            _ = step_tick(period, next) => {
                let Some(period) = period else { continue };
                let now = clock.now();
                let dt_ns = now.time_ns().saturating_sub(last_time_ns);
                last_time_ns = now.time_ns();

                next += period;
                let mut missed_ticks = 0u32;
                if schedule.map(|s| s.missed_tick) == Some(MissedTick::Collapse) {
                    let real_now = tokio::time::Instant::now();
                    while next <= real_now {
                        next += period;
                        missed_ticks = missed_ticks.saturating_add(1);
                    }
                }

                let step = StepContext::new(now.epoch(), step_index, now.time_ns(), dt_ns, missed_ticks);
                step_index += 1;

                // A handler `Err` is a domain outcome: stay healthy, log, continue
                // (D32); the snapshot is committed only after a *successful* step so
                // a failed mutation is never published as committed state. A panic
                // would unwind and abort the process.
                match participant.__step(step).await {
                    Ok(()) => commit_snapshot::<R>(participant, committed),
                    Err(e) => {
                        tracing::warn!(target: "phoxal.runtime", error = %e, "step returned error");
                    }
                }
            }
            Some(incoming) = excl_rx.recv() => {
                // Commit only if the handler succeeded (D14/D32: retain the prior
                // snapshot on a handler error).
                if serve_exclusive_query::<R>(participant, bus, incoming).await {
                    commit_snapshot::<R>(participant, committed);
                }
            }
        }
    }
}

/// Resolve at `next` when there is a step schedule; otherwise never resolve (so
/// the loop is driven only by server queries / shutdown).
async fn step_tick(period: Option<Duration>, next: tokio::time::Instant) {
    match period {
        Some(_) => tokio::time::sleep_until(next).await,
        None => std::future::pending::<()>().await,
    }
}

fn commit_snapshot<R: ParticipantBehavior>(
    participant: &R,
    committed: &Arc<ArcSwapOption<R::Snapshot>>,
) {
    if R::HAS_SNAPSHOT {
        committed.store(Some(Arc::new(participant.__take_snapshot())));
    }
}

/// Serve one exclusive query. Returns `true` iff the handler succeeded (so the
/// runner should commit a fresh snapshot).
async fn serve_exclusive_query<R: ParticipantBehavior>(
    participant: &mut R,
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
    match participant
        .__serve_exclusive(
            &topic,
            &metadata.api_version,
            &metadata.schema_id,
            &metadata.family,
            &request,
        )
        .await
    {
        Ok(reply) => {
            let _ = incoming
                .reply(
                    bus,
                    reply.payload,
                    reply.family,
                    reply.api_version,
                    reply.schema_id,
                )
                .await;
            true
        }
        Err(failure) => {
            let _ = incoming.reply_err(&failure).await;
            false
        }
    }
}

async fn serve_snapshot_query<R: ParticipantBehavior>(
    bus: &Bus,
    incoming: IncomingQuery,
    snapshot: Option<Arc<R::Snapshot>>,
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
    match R::__serve_snapshot(
        snapshot,
        topic,
        metadata.api_version,
        metadata.schema_id,
        metadata.family,
        request,
    )
    .await
    {
        Ok(reply) => {
            let _ = incoming
                .reply(
                    bus,
                    reply.payload,
                    reply.family,
                    reply.api_version,
                    reply.schema_id,
                )
                .await;
        }
        Err(failure) => {
            let _ = incoming.reply_err(&failure).await;
        }
    }
}

async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "failed to listen for ctrl-c");
    }
}

fn init_tracing() {
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
    fn simulation_clock_launch_is_rejected_until_supported() {
        let mut launch = ParticipantLaunch::local("participant", "robot");
        launch.clock = ClockMode::Simulation;

        let Err(err) = launch_clock(&launch) else {
            panic!("simulation clock launch should be rejected");
        };
        let err = err.to_string();
        assert!(err.contains("simulation clock is not yet supported"));
    }
}
