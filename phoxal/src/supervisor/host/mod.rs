//! Own one compiled bundle's execution and answer questions about it.
//!
//! The supervisor reads `manifest.json`, runs the embedded router every
//! participant dials, launches the exact admitted executable graph, watches
//! participant Ready leases, retains logs and telemetry, serves the bundle,
//! and can reboot or power off its host. Required child processes remain
//! owned by this foreground task until they are reaped.
//!
//! Only four things end the run, and every one of them means this process can
//! no longer do its job: the manifest is unreadable, the router cannot bind,
//! the router disappears under it, or the control plane dies. A runtime being
//! absent after the graph became ready is an execution failure: the child
//! graph is stopped, status and logs remain available, and a fresh execution
//! requires an explicit new supervisor start. The supervisor itself remains
//! alive until an operator asks it to stop, so failure evidence is not lost
//! with the required children.
//!
//! # This is an implementation, not an SDK
//!
//! The whole module tree is compiled only by the `supervisor` profile, which
//! the exact-train `phoxal-supervisor` package enables and nothing else does.
//! That package is a `main.rs` over [`run`]: it parses one operand, installs a
//! subscriber, and calls in here. Everything a client has to agree with this
//! process about is [`crate::supervisor::api`] and
//! [`crate::supervisor::rendezvous`], which are ordinary contracts.
//!
//! Because the implementation lives in the same crate as the transport it
//! owns, `BusOwner`, `BusConfig` and the embedded router are crate-private
//! rather than a public seam: there is nothing here for another process to
//! build against.

pub(crate) mod bundle;
pub(crate) mod lock;
pub(crate) mod presence;
pub(crate) mod process;
pub(crate) mod public_backend;
pub(crate) mod router;
pub(crate) mod serve;
pub(crate) mod signal;
pub(crate) mod state;
pub(crate) mod systemd;

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use crate::bus::{
    BusCloseReport, BusConfig, BusHandle, BusOwner, ParticipantReadyEvent,
    ParticipantReadyObserver, ParticipantReadyStatus, SourceLabel,
};
use crate::communication::{DeploymentTarget, SupervisorAdapter};
use crate::communication::session::ExecutionState as PublicExecutionState;
use crate::communication_transport::{PrincipalPolicy, PublicSessionServer, PublicTransportLimits};
use crate::identity::ExecutionId;
use crate::supervisor::api::connect::PRESENCE_KEY;
use crate::supervisor::rendezvous::RuntimeRendezvous;
use tokio_util::sync::CancellationToken;

use presence::Presence;
use state::ExecutionState;
use bundle::Bundle;
use process::ProcessSupervisor;
use public_backend::{
    NoControlledRuntimeBoundary, NoExternalIngress, RuntimePublicBackend, RuntimePublicSurface,
    RuntimeSimulationBridge,
};

const SUPERVISOR_LABEL: &str = "phoxal-supervisor";

/// Observe one compiled bundle's execution until it ends.
///
/// `requested_root` is a bundle directory: `manifest.json`, `assets/`, `bin/`.
/// This is the whole entry point of the `phoxal-supervisor` executable.
///
/// # Errors
///
/// Returns an error when the bundle cannot be opened, the supervisor lock is
/// already held, the embedded router cannot bind or disappears under the run,
/// or the control plane ends unexpectedly.
pub async fn run(requested_root: &Path, target: DeploymentTarget) -> Result<()> {
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
        robot = runtime.robot_id(),
        lock = %lock.path().display(),
        scope = target.scope(),
        supervisor_id = target.supervisor(),
        "phoxal-supervisor starting"
    );

    let state = ExecutionState::new(Presence::for_entries(runtime.expected_processes())?)?;

    let shutdown = CancellationToken::new();
    // Installed before the router is opened, so a signal arriving mid-startup
    // cancels the same token an ordinary stop does. One execution per process,
    // so the handler is never uninstalled.
    signal::cancel_on_termination(shutdown.clone())?;
    let outcome = execute(runtime, &paths, &state, target, shutdown.clone()).await;
    shutdown.cancel();
    outcome
}

async fn execute(
    runtime: Bundle,
    paths: &RuntimeRendezvous,
    state: &ExecutionState,
    target: DeploymentTarget,
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
        }) as self::router::RouterLost
    };
    let router = self::router::start_embedded_router(execution, endpoint.clone(), router_lost)
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

    let public = match start_public_session(&bus, &target, &runtime, state, execution).await {
        Ok(public) => public,
        Err(error) => return Err(abort_router_startup(router, Some(owner), error).await),
    };

    // Declared before the control plane starts serving, so a participant that
    // was already up is seen through the observer's history rather than missed.
    let readiness = match observe_participants(&bus, state).await {
        Ok(readiness) => readiness,
        Err(error) => {
            return finish_startup_error(error, public, identity, owner, router).await;
        }
    };
    // Readiness is the router being up and reachable, which is exactly what
    // this point is. It is deliberately not the graph being complete: systemd
    // orders the runtime units after this one, so a supervisor that withheld
    // READY until they were present would be waiting on units waiting on it.
    let watchdog = match notify_systemd(shutdown.clone()) {
        Ok(watchdog) => watchdog,
        Err(error) => {
            drop(readiness);
            return finish_startup_error(error, public, identity, owner, router).await;
        }
    };

    if shutdown.is_cancelled() {
        drop(readiness);
        drop(identity);
        return finish_startup_stop(public, owner, router, watchdog).await;
    }
    let mut processes = match runtime.source() {
        Some(source) => match ProcessSupervisor::launch(source, &endpoint).await {
            Ok(processes) => Some(processes),
            Err(error) => {
                let error = anyhow::anyhow!(
                    "failed to launch the source bundle runtime graph: {error:#}"
                );
                mark_execution_failed(&public, execution, &error).await;
                let serve_result = serve_until_stop(
                    &bus,
                    state,
                    runtime.root(),
                    runtime.legacy_manifest(),
                    shutdown.clone(),
                )
                .await;
                return finish_failed_execution(
                    error,
                    serve_result,
                    public,
                    owner,
                    router,
                    watchdog,
                    shutdown.clone(),
                )
                .await;
            }
        },
        None => None,
    };
    let process_readiness = if let Some(processes) = processes.as_mut() {
        processes.wait_ready(state, &shutdown).await
    } else {
        Ok(())
    };
    if let Err(error) = process_readiness {
        let intentional_stop = shutdown.is_cancelled();
        if let Some(processes) = processes.as_mut() {
            let _ = processes.stop().await;
        }
        if intentional_stop {
            drop(readiness);
            drop(identity);
            return finish_startup_stop(public, owner, router, watchdog).await;
        }
        mark_execution_failed(&public, execution, &error).await;
        let serve_result = serve_until_stop(
            &bus,
            state,
            runtime.root(),
            runtime.legacy_manifest(),
            shutdown.clone(),
        )
        .await;
        return finish_failed_execution(
            error,
            serve_result,
            public,
            owner,
            router,
            watchdog,
            shutdown.clone(),
        )
        .await;
    }
    if runtime.source().is_some() {
        if let Err(error) = public
            .set_status(
                crate::communication::session::SupervisorState::Ready,
                None,
            )
            .await
        {
            tracing::warn!(error = %error, "failed to publish supervisor Ready status");
        }
        if let Err(error) = public
            .set_execution_state(&execution.to_string(), PublicExecutionState::Ready)
            .await
        {
            tracing::warn!(error = %error, "failed to publish execution Ready state");
        }
    }
    let serve_bundle_root = runtime.root().to_path_buf();
    let serve_manifest = runtime.legacy_manifest();
    let mut process_monitor = processes;
    let outcome = tokio::select! {
        result = serve::serve(
            bus.clone(),
            state.clone(),
            serve_bundle_root.clone(),
            serve_manifest.clone(),
            shutdown.clone(),
        ) => match result {
            Ok(()) if !shutdown.is_cancelled() => Err(anyhow::anyhow!(
                "the supervisor control plane ended unexpectedly"
            )),
            other => other,
        },
        result = async {
            match process_monitor.as_mut() {
                Some(processes) => processes.monitor(&shutdown).await,
                None => std::future::pending().await,
            }
        }, if process_monitor.is_some() => {
            match result {
                Ok(()) if shutdown.is_cancelled() => Ok(()),
                Ok(()) => Err(anyhow::anyhow!("required runtime process monitor ended unexpectedly")),
                Err(error) => {
                    let error = anyhow::anyhow!("required runtime process failed: {error:#}");
                    if let Some(processes) = process_monitor.as_mut()
                        && let Err(stop_error) = processes.stop().await
                    {
                        tracing::warn!(error = %stop_error, "failed to stop all runtime processes after failure");
                    }
                    mark_execution_failed(&public, execution, &error).await;
                    let _ = serve_until_stop(
                        &bus,
                        state,
                        &serve_bundle_root,
                        serve_manifest.clone(),
                        shutdown.clone(),
                    )
                    .await;
                    Err(error)
                }
            }
        }
    };
    shutdown.cancel();

    let process_close = match process_monitor.as_mut() {
        Some(processes) => processes.stop().await,
        None => Ok(()),
    };

    let public_outcome = public.close().await.map_err(anyhow::Error::from);

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
    let outcome = finish_after_transport_close(
        outcome,
        watchdog_outcome,
        public_outcome,
        close,
        router_close,
    );
    process_close?;
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
    public: Result<()>,
    close: BusCloseReport,
    router_close: Result<()>,
) -> Result<()> {
    if !close.is_clean() {
        tracing::warn!(%close, "supervisor bus did not close cleanly");
    }
    if let Err(error) = router_close {
        tracing::warn!(error = %error, "embedded router did not close cleanly");
    }
    outcome.and(watchdog).and(public)
}

/// Start the public session surface on the supervisor's own Zenoh session.
///
/// Source bundles carry the generated artifact summaries that define every
/// public port, bound, and message identity.  The host installs those exact
/// records and wires the session server to the same Runtime bus used by the
/// launched graph.  Legacy observer bundles retain their control-only
/// surface, because they have no typed artifact contract to advertise.
async fn start_public_session(
    bus: &BusHandle,
    target: &DeploymentTarget,
    runtime: &Bundle,
    state: &ExecutionState,
    execution: ExecutionId,
) -> Result<PublicSessionServer> {
    let surface = RuntimePublicSurface::from_bundle(runtime)?;
    let mut adapter = SupervisorAdapter::with_defaults(
        target.clone(),
        env!("CARGO_PKG_VERSION"),
        crate::version::FrameworkVersion::CURRENT_SPELLING,
    )?;
    let timeline = state.time_domain().timeline.to_string();
    adapter.install_execution(surface.execution(
        execution.to_string(),
        timeline,
        PublicExecutionState::Preparing as i32,
    )?)?;
    adapter.set_status(
        crate::communication::session::SupervisorState::Preparing,
        Some("runtime processes are being admitted".to_owned()),
    )?;
    let session = bus.session()?.clone();
    let backend = Arc::new(RuntimePublicBackend::new(
        bus.clone(),
        &surface,
        Arc::new(NoExternalIngress),
    ));
    let simulation_backend = Arc::new(RuntimeSimulationBridge::new(
        bus.clone(),
        &surface,
        None,
        Arc::new(NoControlledRuntimeBoundary),
    ));
    Ok(PublicSessionServer::start_with_backends(
        session,
        adapter,
        backend,
        simulation_backend,
        PrincipalPolicy::Any,
        PublicTransportLimits::default(),
    )
    .await?)
}

async fn serve_until_stop(
    bus: &BusHandle,
    state: &ExecutionState,
    bundle_root: &Path,
    manifest: Option<crate::model::manifest::ManifestDocument>,
    shutdown: CancellationToken,
) -> Result<()> {
    serve::serve(
        bus.clone(),
        state.clone(),
        bundle_root.to_path_buf(),
        manifest,
        shutdown,
    )
    .await
}

async fn mark_execution_failed(
    public: &PublicSessionServer,
    execution: ExecutionId,
    error: &anyhow::Error,
) {
    let detail = format!("{error:#}");
    if let Err(status_error) = public
        .set_status(
            crate::communication::session::SupervisorState::Failed,
            Some(detail),
        )
        .await
    {
        tracing::warn!(error = %status_error, "failed to publish supervisor Failed status");
    }
    if let Err(state_error) = public
        .set_execution_state(&execution.to_string(), PublicExecutionState::Failed)
        .await
    {
        tracing::warn!(error = %state_error, "failed to publish execution Failed state");
    }
    tracing::error!(execution = %execution, error = %error, "required runtime graph failed");
}

async fn finish_startup_stop(
    public: PublicSessionServer,
    owner: BusOwner,
    router: self::router::EmbeddedRouter,
    watchdog: Option<tokio::task::JoinHandle<Result<()>>>,
) -> Result<()> {
    let public = public.close().await.map_err(anyhow::Error::from);
    let watchdog = match watchdog {
        Some(task) => task
            .await
            .context("the systemd watchdog task panicked")
            .and_then(std::convert::identity),
        None => Ok(()),
    };
    let close = owner.close().await;
    let router = router.close().await;
    finish_after_transport_close(Ok(()), watchdog, public, close, router)
}

async fn finish_startup_error(
    failure: anyhow::Error,
    public: PublicSessionServer,
    identity: crate::bus::KeyLivelinessToken,
    owner: BusOwner,
    router: self::router::EmbeddedRouter,
) -> Result<()> {
    drop(identity);
    let public = public.close().await.map_err(anyhow::Error::from);
    let close = owner.close().await;
    let router = router.close().await;
    finish_after_transport_close(Err(failure), Ok(()), public, close, router)
}

async fn finish_failed_execution(
    failure: anyhow::Error,
    served: Result<()>,
    public: PublicSessionServer,
    owner: BusOwner,
    router: self::router::EmbeddedRouter,
    watchdog: Option<tokio::task::JoinHandle<Result<()>>>,
    shutdown: CancellationToken,
) -> Result<()> {
    if let Err(error) = served {
        tracing::warn!(error = %error, "failed execution could not keep its diagnostic surface alive");
    }
    shutdown.cancel();
    let public = public.close().await.map_err(anyhow::Error::from);
    let watchdog = match watchdog {
        Some(task) => task
            .await
            .context("the systemd watchdog task panicked")
            .and_then(std::convert::identity),
        None => Ok(()),
    };
    let close = owner.close().await;
    let router = router.close().await;
    let cleanup = finish_after_transport_close(Ok(()), watchdog, public, close, router);
    if let Err(error) = cleanup {
        tracing::warn!(error = %error, "failed execution cleanup was not fully clean");
    }
    Err(failure)
}

async fn abort_router_startup(
    router: self::router::EmbeddedRouter,
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
    let notify = self::systemd::notify::SdNotify::from_env().unwrap_or_else(|error| {
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
    use crate::bus::BusCloseTimeout;

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
            Ok(()),
            timed_out(),
            Err(anyhow::anyhow!("router close failed")),
        )
        .expect_err("the watchdog failure remains authoritative");
        assert_eq!(error.to_string(), "the watchdog notification failed");
    }
}
