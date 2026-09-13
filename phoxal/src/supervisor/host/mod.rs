//! Supervisor-owned source-bundle execution host.
//!
//! The host admits one immutable source bundle, starts the embedded router,
//! launches its recorded Runtime processes, and serves the public session on
//! the same execution transport. Runtime admission is the readiness proof.
//! There is no legacy observer, MessagePack control plane, or second serving
//! path.

pub(crate) mod bundle;
pub(crate) mod execution;
pub(crate) mod lock;
pub(crate) mod process;
pub(crate) mod public_backend;
pub(crate) mod router;
pub(crate) mod signal;
pub(crate) mod state;
pub(crate) mod systemd;

use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::bus::{BusConfig, BusHandle, BusOwner};
use crate::communication::session::ExecutionState as PublicExecutionState;
use crate::communication::{DeploymentTarget, SupervisorAdapter};
use crate::communication_transport::{PrincipalPolicy, PublicSessionServer, PublicTransportLimits};
use crate::identity::ExecutionId;
use crate::supervisor::rendezvous::RuntimeRendezvous;
use anyhow::{Context, Result, bail};
use tokio_util::sync::CancellationToken;

use bundle::Bundle;
use execution::{RuntimeExecutionMode, RuntimeExecutionProtocol};
use process::ProcessSupervisor;
use public_backend::{RuntimeExecutionCoordinator, RuntimePublicBackend, RuntimePublicSurface};
use state::{ExecutionState, TimeMode};

/// Execute one compiled source bundle until the supervisor or its required
/// Runtime graph stops.
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

    let state = ExecutionState::new();
    let shutdown = CancellationToken::new();
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
    let source = runtime
        .source()
        .ok_or_else(|| anyhow::anyhow!("compiled bundle has no source execution graph"))?;
    let endpoint = router_endpoint(&paths.checked_supervisor_socket()?);
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

    let (owner, bus) = match BusOwner::open(BusConfig::for_external(
        execution,
        None,
        vec![endpoint.clone()],
    ))
    .await
    {
        Ok(opened) => opened,
        Err(error) => {
            return Err(abort_router_startup(
                router,
                anyhow::anyhow!("failed to open supervisor bus: {error}"),
            )
            .await);
        }
    };
    if let Err(error) = verify_router_identity(&bus, execution, &endpoint).await {
        let _ = owner.close().await;
        let _ = router.close().await;
        return Err(error);
    }

    let surface = match RuntimePublicSurface::from_bundle(&runtime) {
        Ok(surface) => surface,
        Err(error) => {
            let _ = owner.close().await;
            let _ = router.close().await;
            return Err(error);
        }
    };
    if surface.simulation.is_some() && state.time_domain().mode != TimeMode::Simulated {
        state
            .replace_time_domain(TimeMode::Simulated)
            .map_err(anyhow::Error::msg)?;
    }
    let protocol = Arc::new(
        RuntimeExecutionProtocol::open(bus.clone(), source, state.clone())
            .await
            .context("failed to open Runtime execution protocol")?,
    );
    let public = match start_public_session(
        &bus,
        &target,
        &surface,
        state,
        execution,
        Arc::clone(&protocol),
    )
    .await
    {
        Ok(public) => public,
        Err(error) => {
            let _ = owner.close().await;
            let _ = router.close().await;
            return Err(error);
        }
    };
    let watchdog = match notify_systemd(shutdown.clone()) {
        Ok(watchdog) => watchdog,
        Err(error) => {
            let _ = public.close().await;
            let _ = owner.close().await;
            let _ = router.close().await;
            return Err(error);
        }
    };

    let mut processes = match ProcessSupervisor::launch(source, execution, &endpoint).await {
        Ok(processes) => processes,
        Err(error) => {
            let error = anyhow::anyhow!("failed to launch the Runtime graph: {error:#}");
            mark_execution_failed(&public, execution, &error).await;
            return finish_run(
                Err(error),
                None,
                public,
                owner,
                router,
                watchdog,
                shutdown,
                router_loss,
            )
            .await;
        }
    };

    let (mode, quantum_ns) = match surface.simulation.as_ref() {
        Some(definition) => (RuntimeExecutionMode::Controlled, definition.quantum_ns()),
        None => (RuntimeExecutionMode::Hardware, 0),
    };
    if let Err(error) = protocol
        .admit_all(mode, quantum_ns, &state.time_domain().timeline.to_string())
        .await
    {
        let error = anyhow::anyhow!("Runtime execution admission failed: {error:#}");
        let _ = processes.stop().await;
        mark_execution_failed(&public, execution, &error).await;
        return finish_run(
            Err(error),
            None,
            public,
            owner,
            router,
            watchdog,
            shutdown,
            router_loss,
        )
        .await;
    }
    state.mark_ready();
    let _ = public
        .set_status(crate::communication::session::SupervisorState::Ready, None)
        .await;
    let _ = public
        .set_execution_state(&execution.to_string(), PublicExecutionState::Ready)
        .await;

    let outcome = tokio::select! {
        failure = async {
            protocol.wait_failed().await;
            protocol.failure_reason().await
        }, if matches!(mode, RuntimeExecutionMode::Controlled) => {
            let error = anyhow::anyhow!(
                "controlled Runtime boundary failed: {}",
                failure.unwrap_or_else(|| "unspecified boundary failure".to_owned())
            );
            mark_execution_failed(&public, execution, &error).await;
            Err(error)
        }
        () = shutdown.cancelled() => Ok(()),
        result = processes.monitor(&shutdown) => {
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let error = anyhow::anyhow!("required Runtime process failed: {error:#}");
                    mark_execution_failed(&public, execution, &error).await;
                    Err(error)
                }
            }
        }
    };
    finish_run(
        outcome,
        Some(processes),
        public,
        owner,
        router,
        watchdog,
        shutdown,
        router_loss,
    )
    .await
}

async fn start_public_session(
    bus: &BusHandle,
    target: &DeploymentTarget,
    surface: &RuntimePublicSurface,
    state: &ExecutionState,
    execution: ExecutionId,
    protocol: Arc<RuntimeExecutionProtocol>,
) -> Result<PublicSessionServer> {
    let coordinator = Arc::new(RuntimeExecutionCoordinator::new(state.clone()));
    let mut adapter = SupervisorAdapter::with_defaults(
        target.clone(),
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION"),
    )?;
    let timeline = state.time_domain().timeline.to_string();
    adapter.install_execution(surface.execution(
        execution.to_string(),
        timeline,
        PublicExecutionState::Preparing as i32,
    )?)?;
    adapter.set_status(
        crate::communication::session::SupervisorState::Preparing,
        Some("Runtime processes are being admitted".to_owned()),
    )?;
    let session = bus.session()?.clone();
    let backend = Arc::new(RuntimePublicBackend::new(bus.clone(), surface, coordinator));
    match surface.simulation.clone() {
        Some(definition) => Ok(PublicSessionServer::start_with_backends(
            session,
            adapter,
            backend,
            Arc::new(public_backend::RuntimeSimulationBridge::new(
                bus.clone(),
                surface,
                Some(definition),
                protocol,
            )),
            PrincipalPolicy::Any,
            PublicTransportLimits::default(),
        )
        .await?),
        None => Ok(PublicSessionServer::start_with_backend(
            session,
            adapter,
            backend,
            PrincipalPolicy::Any,
            PublicTransportLimits::default(),
        )
        .await?),
    }
}

async fn mark_execution_failed(
    public: &PublicSessionServer,
    execution: ExecutionId,
    error: &anyhow::Error,
) {
    let detail = format!("{error:#}");
    let _ = public
        .set_status(
            crate::communication::session::SupervisorState::Failed,
            Some(detail),
        )
        .await;
    let _ = public
        .set_execution_state(&execution.to_string(), PublicExecutionState::Failed)
        .await;
}

async fn finish_run(
    outcome: Result<()>,
    mut processes: Option<ProcessSupervisor>,
    public: PublicSessionServer,
    owner: BusOwner,
    router: self::router::EmbeddedRouter,
    watchdog: Option<tokio::task::JoinHandle<Result<()>>>,
    shutdown: CancellationToken,
    router_loss: Arc<OnceLock<String>>,
) -> Result<()> {
    shutdown.cancel();
    let process_outcome = match processes.as_mut() {
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
    let close = owner.close().await;
    if !close.is_clean() {
        tracing::warn!(%close, "supervisor bus did not close cleanly");
    }
    if let Err(error) = router.close().await {
        tracing::warn!(error = %error, "embedded router did not close cleanly");
    }
    process_outcome?;
    outcome.and(watchdog_outcome).and(public_outcome)?;
    if let Some(reason) = router_loss.get() {
        bail!("{reason}");
    }
    Ok(())
}

async fn abort_router_startup(
    router: self::router::EmbeddedRouter,
    error: anyhow::Error,
) -> anyhow::Error {
    if let Err(close_error) = router.close().await {
        tracing::warn!(error = %close_error, "embedded router did not close after startup failure");
    }
    error
}

/// Tell systemd the supervisor is up, and keep its watchdog fed until stop.
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
