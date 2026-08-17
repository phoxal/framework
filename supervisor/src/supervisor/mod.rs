//! Watch one compiled bundle's execution and answer questions about it.
//!
//! The supervisor reads `manifest.json`, runs the embedded router every
//! participant dials, watches participant Ready leases, retains logs and
//! telemetry, serves the bundle, and can reboot or power off its host. It
//! starts nothing and stops nothing: runtimes are launched by `phoxal` locally
//! and by systemd on a device, and each of those already owns the child facts
//! of what it started.
//!
//! Only four things end the run, and every one of them means this process can
//! no longer do its job: the manifest is unreadable, the router cannot bind,
//! the router disappears under it, or the control plane dies. A runtime being
//! absent is none of those - it is the `Degraded` lifecycle, published and
//! waited on. There is no failure document either: all four fatal conditions
//! take the transport with them, so nothing this process published on the way
//! down would reach anyone. What a client sees instead is the
//! `supervisor/presence` liveliness token disappearing, which is the one piece
//! of evidence that survives its author.

pub(crate) mod bundle;
pub(crate) mod lock;
pub(crate) mod presence;
pub(crate) mod serve;
pub(crate) mod signal;
pub(crate) mod state;

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use phoxal_bus::{
    BusCloseReport, BusConfig, BusHandle, BusOwner, ParticipantReadyEvent,
    ParticipantReadyObserver, ParticipantReadyStatus, SourceLabel,
};
use phoxal_protocol::supervisor::connect::PRESENCE_KEY;
use phoxal_runtime_contract::identity::ExecutionId;
use phoxal_runtime_contract::rendezvous::RuntimeRendezvous;
use tokio_util::sync::CancellationToken;

use presence::Presence;
use state::ExecutionState;

const SUPERVISOR_LABEL: &str = "phoxal-supervisor";

pub(crate) async fn run(requested_root: &Path) -> Result<()> {
    let canonical = requested_root.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize bundle root {}",
            requested_root.display()
        )
    })?;
    let paths = RuntimeRendezvous::for_root(&bundle::owning_root(&canonical));
    let lock = lock::SupervisorLock::acquire(&paths.supervisor_lock())?;
    let runtime = bundle::open(&canonical)?;
    tracing::info!(
        bundle = %runtime.root().display(),
        lock = %lock.path().display(),
        "phoxal-supervisor starting"
    );

    let state = ExecutionState::new(Presence::for_robot(runtime.robot()));

    let shutdown = CancellationToken::new();
    // Installed before the router is opened, so a signal arriving mid-startup
    // cancels the same token an ordinary stop does. One execution per process,
    // so the handler is never uninstalled.
    signal::cancel_on_termination(shutdown.clone())?;
    let outcome = execute(runtime, &paths, &state, shutdown.clone()).await;
    shutdown.cancel();
    outcome
}

async fn execute(
    runtime: phoxal_bundle::RuntimeBundle,
    paths: &RuntimeRendezvous,
    state: &ExecutionState,
    shutdown: CancellationToken,
) -> Result<()> {
    let execution = ExecutionId::mint();
    let endpoint = router_endpoint(&paths.checked_supervisor_socket()?);
    // The loss reason is recorded rather than published: by the time it is
    // known the fabric every client reaches this process through is already
    // gone, so the only place left to report it is this process's own exit.
    let router_loss: Arc<OnceLock<String>> = Arc::default();
    let router_lost = {
        let router_loss = Arc::clone(&router_loss);
        let shutdown = shutdown.clone();
        Arc::new(move |reason: String| {
            let _ = router_loss.set(reason);
            shutdown.cancel();
        }) as crate::router::RouterLost
    };
    let router = crate::router::start_embedded_router(execution, endpoint.clone(), router_lost)
        .await
        .context("the embedded router did not start")?;

    let label = match SourceLabel::new(SUPERVISOR_LABEL) {
        Ok(label) => label,
        Err(error) => return Err(abort_router_startup(router, None, error.into()).await),
    };
    let (owner, bus) = match BusOwner::open(BusConfig::for_external(
        execution,
        Some(label),
        vec![endpoint.clone()],
    ))
    .await
    {
        Ok(opened) => opened,
        Err(error) => {
            let error = anyhow::anyhow!("failed to open supervisor bus: {error}");
            return Err(abort_router_startup(router, None, error).await);
        }
    };
    if let Err(error) = verify_router_identity(&bus, execution, &endpoint).await {
        return Err(abort_router_startup(router, Some(owner), error).await);
    }
    // The one token that answers "is this supervisor still here" to a client
    // that can no longer be told anything.
    let identity = match owner.declare_liveliness_key(PRESENCE_KEY).await {
        Ok(identity) => identity,
        Err(error) => return Err(abort_router_startup(router, Some(owner), error.into()).await),
    };
    tracing::info!(%execution, endpoint = %endpoint, "supervisor control plane is up");

    // Declared before the control plane starts serving, so a participant that
    // was already up is seen through the observer's history rather than missed.
    let readiness = observe_participants(&bus, state).await?;
    // Readiness is the router being up and reachable, which is exactly what
    // this point is. It is deliberately not the graph being complete: systemd
    // orders the runtime units after this one, so a supervisor that withheld
    // READY until they were present would be waiting on units waiting on it.
    let watchdog = notify_systemd(shutdown.clone())?;

    let outcome = match serve::serve(bus.clone(), state.clone(), runtime, shutdown.clone()).await {
        Ok(()) if !shutdown.is_cancelled() => Err(anyhow::anyhow!(
            "the supervisor control plane ended unexpectedly"
        )),
        other => other,
    };
    shutdown.cancel();

    let watchdog_outcome = match watchdog {
        Some(task) => task
            .await
            .context("the systemd watchdog task panicked")
            .and_then(std::convert::identity),
        None => Ok(()),
    };
    drop(readiness);
    drop(identity);
    let close = owner.close().await;
    let router_close = router.close().await;
    let outcome = finish_after_transport_close(outcome, watchdog_outcome, close, router_close);
    match router_loss.get() {
        Some(reason) => Err(anyhow::anyhow!("{reason}")),
        None => outcome,
    }
}

/// Preserve the run's result across the terminal transport cleanup.
///
/// Closing the supervisor's session and embedded router is bounded,
/// best-effort cleanup on the exit path. Its evidence stays diagnostic: it
/// cannot turn a completed run into a failed one. Failures raised while the
/// supervisor was still serving remain fatal.
fn finish_after_transport_close(
    outcome: Result<()>,
    watchdog: Result<()>,
    close: BusCloseReport,
    router_close: Result<()>,
) -> Result<()> {
    if !close.is_clean() {
        tracing::warn!(%close, "supervisor bus did not close cleanly");
    }
    if let Err(error) = router_close {
        tracing::warn!(error = %error, "embedded router did not close cleanly");
    }
    outcome.and(watchdog)
}

async fn abort_router_startup(
    router: crate::router::EmbeddedRouter,
    owner: Option<BusOwner>,
    error: anyhow::Error,
) -> anyhow::Error {
    if let Some(owner) = owner {
        let close = owner.close().await;
        if !close.is_clean() {
            tracing::warn!(%close, "supervisor bus did not close cleanly after startup failed");
        }
    }
    if let Err(close_error) = router.close().await {
        tracing::warn!(error = %close_error, "embedded router did not close cleanly after startup failed");
    }
    error
}

async fn observe_participants(
    bus: &BusHandle,
    state: &ExecutionState,
) -> Result<ParticipantReadyObserver> {
    let state = state.clone();
    Ok(bus
        .observe_participant_ready(move |event: ParticipantReadyEvent| {
            state.record_presence(
                event.participant(),
                event.producer(),
                event.status == ParticipantReadyStatus::Ready,
            );
        })
        .await?)
}

/// Tell systemd the supervisor is up, and keep the watchdog fed until the run
/// ends. A run outside systemd has no notify socket and nothing to do here.
fn notify_systemd(
    shutdown: CancellationToken,
) -> Result<Option<tokio::task::JoinHandle<Result<()>>>> {
    let notify = crate::systemd::notify::SdNotify::from_env().unwrap_or_else(|error| {
        tracing::warn!("ignoring an unusable systemd notify socket: {error:#}");
        None
    });
    let Some(notify) = notify else {
        return Ok(None);
    };
    notify.notify_ready()?;
    let Some(interval) = notify.watchdog_interval() else {
        return Ok(None);
    };
    Ok(Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                _ = ticker.tick() => notify.notify_watchdog()?,
            }
        }
    })))
}

fn router_endpoint(socket: &Path) -> String {
    format!("unixsock-stream/{}", socket.display())
}

async fn verify_router_identity(
    bus: &BusHandle,
    expected: ExecutionId,
    endpoint: &str,
) -> Result<()> {
    let executions = BusOwner::probe_routers(endpoint).await?;
    match executions.as_slice() {
        [reported] if *reported == expected => {}
        [reported] => bail!("router reports {reported}, expected {expected}"),
        [] => bail!("router on {endpoint} reports no execution identity"),
        many => bail!("router endpoint {endpoint} reports {} routers", many.len()),
    }
    anyhow::ensure!(
        bus.execution() == expected,
        "supervisor bus execution mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use phoxal_bus::BusCloseTimeout;

    use super::*;

    fn timed_out() -> BusCloseReport {
        BusCloseReport {
            timed_out: vec![BusCloseTimeout::Session],
            ..BusCloseReport::default()
        }
    }

    #[test]
    fn terminal_transport_cleanup_does_not_fail_a_completed_run() {
        finish_after_transport_close(
            Ok(()),
            Ok(()),
            timed_out(),
            Err(anyhow::anyhow!("router close failed")),
        )
        .expect("terminal transport cleanup is diagnostic");
    }

    #[test]
    fn transport_cleanup_does_not_mask_a_serving_failure() {
        let error = finish_after_transport_close(
            Err(anyhow::anyhow!("the supervisor control plane failed")),
            Ok(()),
            timed_out(),
            Err(anyhow::anyhow!("router close failed")),
        )
        .expect_err("the serving failure remains authoritative");
        assert_eq!(error.to_string(), "the supervisor control plane failed");
    }

    #[test]
    fn transport_cleanup_does_not_mask_a_watchdog_failure() {
        let error = finish_after_transport_close(
            Ok(()),
            Err(anyhow::anyhow!("the watchdog notification failed")),
            timed_out(),
            Err(anyhow::anyhow!("router close failed")),
        )
        .expect_err("the watchdog failure remains authoritative");
        assert_eq!(error.to_string(), "the watchdog notification failed");
    }
}
