//! The runner (D23/D34): owns the bus connection, the clock, step scheduling,
//! server-query dispatch, snapshot commits, and graceful shutdown.
//!
//! `phoxal::run::<R>()` builds a blocking Tokio runtime and runs the runtime to
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

use crate::bus::{Bus, BusConfig, IncomingQuery, QueryFailure};
use crate::runtime::clock::{ClockSource, RealClock};
use crate::runtime::context::{SetupContext, ShutdownContext, StepContext};
use crate::runtime::emit::print_emit_apis;
use crate::runtime::launch::ParticipantLaunch;
use crate::runtime::spec::{MissedTick, RuntimeBehavior, StepSchedule};

/// Run a runtime to completion on a framework-owned blocking Tokio runtime.
///
/// The default binary entrypoint:
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Runtime>() }`.
pub fn run<R: RuntimeBehavior>() -> crate::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_async::<R>())
}

/// Async host runner for custom Tokio mains
/// (`phoxal::tokio::run::<Runtime>().await`).
pub async fn run_async<R: RuntimeBehavior>() -> crate::Result<()> {
    // The `emit-apis` subcommand short-circuits before config / `.env` / tracing /
    // Zenoh / setup — the compiled-in metadata is authoritative (D50).
    if std::env::args().nth(1).as_deref() == Some("emit-apis") {
        print_emit_apis::<R>();
        return Ok(());
    }

    init_tracing();

    let launch = ParticipantLaunch::local(R::ID, "robot");
    run_with::<R, _, _>(launch, RealClock::new(), shutdown_signal()).await
}

/// Run a runtime against an explicit launch, clock, and shutdown trigger. The
/// seam the test harness + integration tests drive (D41).
pub async fn run_with<R, C, S>(
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: RuntimeBehavior,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let bus = Bus::open(BusConfig {
        namespace: launch.namespace.clone(),
        robot_id: launch.robot_id.clone(),
        participant: launch.participant_id.clone(),
        incarnation: 0,
        connect_endpoints: launch.bus.connect_endpoints.clone(),
    })
    .await?;

    let result = run_lifecycle::<R, C, S>(&bus, launch, clock, shutdown).await;

    if let Err(e) = bus.close().await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "bus close failed");
    }
    result
}

async fn run_lifecycle<R, C, S>(
    bus: &Bus,
    launch: ParticipantLaunch,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: RuntimeBehavior,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let config: R::Config = match &launch.config {
        Some(value) => serde_json::from_value(value.clone())?,
        None => serde_json::from_value(serde_json::Value::Null)?,
    };

    let mut ctx = SetupContext::<R>::new(bus.clone());
    let mut runtime = R::__setup(&mut ctx, config).await?;
    tracing::info!(target: "phoxal.runtime", id = R::ID, participant = %launch.participant_id, "runtime ready");

    // Committed snapshot, shared with concurrent snapshot-server tasks (D16).
    let committed: Arc<ArcSwapOption<R::Snapshot>> = Arc::new(ArcSwapOption::empty());
    commit_snapshot::<R>(&runtime, &committed);

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

    // Concurrent snapshot-server queries are spawned per query against the latest
    // committed snapshot.
    for topic in R::__snapshot_server_topics() {
        let queryable = bus.declare_server(topic).await?;
        let committed = Arc::clone(&committed);
        let bus = bus.clone();
        server_tasks.push(tokio::spawn(async move {
            while let Ok(incoming) = queryable.recv().await {
                let snapshot = committed.load_full();
                let bus = bus.clone();
                tokio::spawn(
                    async move { serve_snapshot_query::<R>(&bus, incoming, snapshot).await },
                );
            }
        }));
    }

    let schedule = R::__step_schedule();
    let shutdown = pin!(shutdown);
    main_loop::<R, C, S>(
        &mut runtime,
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

    let grace = Duration::from_millis(launch.shutdown_grace_ms);
    if let Err(e) = runtime.__shutdown(ShutdownContext::new(grace)).await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "shutdown hook returned error");
    }
    tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn main_loop<R, C, S>(
    runtime: &mut R,
    bus: &Bus,
    clock: &C,
    schedule: Option<StepSchedule>,
    committed: &Arc<ArcSwapOption<R::Snapshot>>,
    excl_rx: &mut mpsc::Receiver<IncomingQuery>,
    mut shutdown: std::pin::Pin<&mut S>,
) where
    R: RuntimeBehavior,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let period = schedule.map(|s| s.period());
    let mut step_index: u64 = 0;
    let mut last_time_ns = clock.now().time_ns();
    let mut next = tokio::time::Instant::now() + period.unwrap_or_else(|| Duration::from_secs(1));

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return,
            incoming = excl_rx.recv() => {
                if let Some(incoming) = incoming {
                    serve_exclusive_query::<R>(runtime, bus, incoming).await;
                    commit_snapshot::<R>(runtime, committed);
                }
            }
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
                // (D32). A panic would unwind and abort the process.
                if let Err(e) = runtime.__step(step).await {
                    tracing::warn!(target: "phoxal.runtime", error = %e, "step returned error");
                }
                commit_snapshot::<R>(runtime, committed);
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

fn commit_snapshot<R: RuntimeBehavior>(runtime: &R, committed: &Arc<ArcSwapOption<R::Snapshot>>) {
    if R::HAS_SNAPSHOT {
        committed.store(Some(Arc::new(runtime.__take_snapshot())));
    }
}

async fn serve_exclusive_query<R: RuntimeBehavior>(
    runtime: &mut R,
    bus: &Bus,
    incoming: IncomingQuery,
) {
    let topic = incoming.topic_key().to_string();
    let request = match incoming.request_bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = incoming
                .reply_err(&QueryFailure::internal(e.to_string()))
                .await;
            return;
        }
    };
    match runtime.__serve_exclusive(&topic, &request).await {
        Ok(reply) => {
            let _ = incoming
                .reply(bus, reply.payload, reply.family, reply.api_version)
                .await;
        }
        Err(failure) => {
            let _ = incoming.reply_err(&failure).await;
        }
    }
}

async fn serve_snapshot_query<R: RuntimeBehavior>(
    bus: &Bus,
    incoming: IncomingQuery,
    snapshot: Option<Arc<R::Snapshot>>,
) {
    let topic = incoming.topic_key().to_string();
    let request = match incoming.request_bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = incoming
                .reply_err(&QueryFailure::internal(e.to_string()))
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
    match R::__serve_snapshot(snapshot, topic, request).await {
        Ok(reply) => {
            let _ = incoming
                .reply(bus, reply.payload, reply.family, reply.api_version)
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
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .try_init();
    });
}
