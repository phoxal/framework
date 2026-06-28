//! The runner (D23/D34): owns the bus connection, the clock, step scheduling,
//! and graceful shutdown.
//!
//! `phoxal::run::<R>()` builds a blocking Tokio runtime and runs the runtime to
//! completion; `phoxal::tokio::run::<R>().await` is the async entrypoint for
//! custom Tokio mains. The first slice owns step scheduling + clean shutdown; the
//! heavier runner pieces (hard watchdog, managed tasks, health aggregation) are
//! sequenced into later slices.

use std::future::Future;
use std::pin::pin;
use std::sync::OnceLock;
use std::time::Duration;

use crate::bus::{Bus, BusConfig};
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

    let schedule = R::__step_schedule();
    let shutdown = pin!(shutdown);
    step_loop::<R, C, S>(&mut runtime, &clock, schedule, shutdown).await;

    let grace = Duration::from_millis(launch.shutdown_grace_ms);
    if let Err(e) = runtime.__shutdown(ShutdownContext::new(grace)).await {
        tracing::warn!(target: "phoxal.runtime", error = %e, "shutdown hook returned error");
    }
    tracing::info!(target: "phoxal.runtime", id = R::ID, "runtime stopped");
    Ok(())
}

async fn step_loop<R, C, S>(
    runtime: &mut R,
    clock: &C,
    schedule: Option<StepSchedule>,
    mut shutdown: std::pin::Pin<&mut S>,
) where
    R: RuntimeBehavior,
    C: ClockSource,
    S: Future<Output = ()>,
{
    let Some(schedule) = schedule else {
        // Server-only / no scheduled loop: wait for shutdown.
        (&mut shutdown).await;
        return;
    };

    let period = schedule.period();
    let mut step_index: u64 = 0;
    let mut last_time_ns = clock.now().time_ns();
    let mut next = tokio::time::Instant::now() + period;

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return,
            _ = tokio::time::sleep_until(next) => {
                let now = clock.now();
                let dt_ns = now.time_ns().saturating_sub(last_time_ns);
                last_time_ns = now.time_ns();

                // Advance the deadline; collapse overruns into one step (D34).
                next += period;
                let mut missed_ticks = 0u32;
                let real_now = tokio::time::Instant::now();
                if schedule.missed_tick == MissedTick::Collapse {
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
            }
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
