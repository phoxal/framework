//! Supervisor-owned source-bundle execution host.
//!
//! The host admits one immutable source bundle, starts the embedded router,
//! launches its recorded Runtime processes, and serves the public session on
//! the same execution transport. Runtime admission is the readiness proof.
//! There is no legacy observer, MessagePack control plane, or second serving
//! path.

pub(crate) mod adapter;
pub(crate) mod bundle;
pub(crate) mod execution;
pub(crate) mod lock;
pub(crate) mod process;
pub(crate) mod public_backend;
pub(crate) mod router;
pub(crate) mod session_table;
pub(crate) mod signal;
pub(crate) mod state;
pub(crate) mod systemd;
pub(crate) mod transport;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::rendezvous::RuntimeRendezvous;
use anyhow::{Context, Result, bail};
use phoxal::communication::DeploymentTarget;
use phoxal::communication::session::ExecutionState as PublicExecutionState;
use phoxal::communication_transport::PublicTransportLimits;

use self::adapter::SupervisorAdapter;
use self::transport::server::{PrincipalPolicy, PublicSessionServer};
use crate::scenario_admission::{
    ScenarioLaunchMode, admission_diagnostic, evaluate_scenario_admission,
};
use phoxal::identity::ExecutionId;
use phoxal::runtime::connection::{Connection, ConnectionConfig, ConnectionOwner};
use tokio_util::sync::CancellationToken;

use bundle::Bundle;
use execution::{RuntimeExecutionMode, RuntimeExecutionProtocol};
use process::ProcessSupervisor;
use public_backend::{RuntimeExecutionCoordinator, RuntimePublicBackend, RuntimePublicSurface};
use state::{ExecutionState, TimeMode};

struct ExecutionLaunch {
    runtime: Bundle,
    endpoint: String,
    target: DeploymentTarget,
    ready_file: Option<PathBuf>,
    scenario_result: Option<PathBuf>,
    shutdown: CancellationToken,
    scenario_program: Option<phoxal::scenario::Program>,
}

/// Execute one compiled source bundle and publish readiness atomically.
///
/// The optional file is an internal local-orchestration handoff. It becomes
/// visible only after every required Runtime has completed admission and the
/// public execution status is Ready.
pub async fn run(
    requested_root: &Path,
    target: DeploymentTarget,
    ready_file: Option<&Path>,
    scenario_result: Option<&Path>,
    listen: Option<&str>,
    launch_mode: ScenarioLaunchMode,
) -> Result<()> {
    let canonical = requested_root.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize bundle root {}",
            requested_root.display()
        )
    })?;
    let paths = RuntimeRendezvous::for_root(&bundle::owning_root(&canonical));
    let lock = lock::SupervisorLock::acquire(&paths.supervisor_lock())?;
    let runtime = bundle::open(&canonical)?;
    let mut scenario_program = None;
    if let Some(marker_value) = runtime.scenario_marker() {
        // Scenario bundles must carry a validated program identity inside
        // the nested `scenario` section. Verify the bounded program
        // bytes against the recorded length and SHA-256 digest before
        // consulting the admission policy; refuse inconsistent launch
        // modes; never substitute placeholder identity values.
        let section = runtime.scenario_section().ok_or_else(|| {
            anyhow::anyhow!(
                "scenario bundle carries `{marker_value}` but no nested `scenario` section \
                 was written; refusing to launch"
            )
        })?;
        let program = &section.program;
        let bytes = program.verify_against(&canonical).with_context(|| {
            format!(
                "scenario program `{}` failed admission",
                program.scenario_name
            )
        })?;
        // The bytes the fixture later consumes are exactly the
        // bytes the supervisor verified — decode the program here so
        // a tampered or malformed artifact is rejected before any
        // child starts. The fixture re-decodes at its own admission
        // boundary.
        let decoded = phoxal::scenario::Program::decode(&bytes)
            .map_err(|error| anyhow::anyhow!("scenario program decode failed: {error}"))?;
        decoded
            .verify_identity()
            .map_err(|error| anyhow::anyhow!("scenario program identity check failed: {error}"))?;
        // The scenario name must match what the program itself
        // carries. A scenario_name mismatch means the case host
        // mis-wired the bundle or a tampered bundle substituted an
        // unrelated program.
        if decoded.scenario_name() != program.scenario_name {
            return Err(anyhow::anyhow!(
                "scenario program `{}` declared scenario_name `{}`; refusing to admit \
                 a bundle whose program identity disagrees with the manifest",
                program.scenario_name,
                decoded.scenario_name(),
            ));
        }
        if !program.controlled_execution {
            return Err(anyhow::anyhow!(
                "scenario bundle `{}` declares a non-controlled execution mode; \
                 controlled simulation is the only supported scenario launch mode",
                program.scenario_name
            ));
        }
        if matches!(launch_mode, ScenarioLaunchMode::Hardware) {
            return Err(anyhow::anyhow!(
                "scenario bundle `{}` carries the nondeployable marker; \
                 refusing hardware launch mode",
                program.scenario_name
            ));
        }
        if runtime.simulation().is_none() {
            return Err(anyhow::anyhow!(
                "scenario bundle `{}` requires a controlled simulation definition; \
                 refusing to launch without one",
                program.scenario_name
            ));
        }
        // Validate quantum/bound alignment: the controlled
        // simulation's quantum and transition bounds must agree with
        // the decoded program. Presence alone is insufficient.
        //
        // The supervisor and program both reason about the quantum in
        // nanoseconds: comparing the simulation's `quantum_ns` against
        // `program.quantum().micros() * 1_000` avoids the integer
        // truncation that would otherwise accept `2_000_001 ns`
        // against a `2_000 us` program.
        let simulation = runtime.simulation().ok_or_else(|| {
            anyhow::anyhow!("scenario bundle must declare a controlled simulation")
        })?;
        validate_simulation_quantum(
            &program.scenario_name,
            decoded.quantum().micros(),
            simulation.quantum_ns,
        )
        .map_err(|mismatch| anyhow::anyhow!("{mismatch}"))?;
        let admission = evaluate_scenario_admission(
            launch_mode,
            &program.scenario_name,
            program.program_byte_length,
            &program.program_digest,
            Some(marker_value.as_str()),
        );
        if let Some(diagnostic) = admission_diagnostic(&admission) {
            return Err(anyhow::anyhow!(
                "scenario bundle refused by supervisor admission policy: {diagnostic}"
            ));
        }
        scenario_program = Some(decoded);
    }
    tracing::info!(
        bundle = %runtime.root().display(),
        robot = runtime.robot_id(),
        lock = %lock.path().display(),
        scope = target.scope(),
        supervisor_id = target.supervisor(),
        launch_mode = ?launch_mode,
        "phoxal-supervisor starting"
    );

    let state = ExecutionState::new();
    let shutdown = CancellationToken::new();
    signal::cancel_on_termination(shutdown.clone())?;
    let endpoint = match listen {
        Some(endpoint) => endpoint.to_owned(),
        None => router_endpoint(&paths.checked_supervisor_socket()?),
    };
    let outcome = execute(
        ExecutionLaunch {
            runtime,
            endpoint,
            target,
            ready_file: ready_file.map(Path::to_owned),
            scenario_result: scenario_result.map(Path::to_owned),
            shutdown: shutdown.clone(),
            scenario_program,
        },
        &state,
    )
    .await;
    shutdown.cancel();
    outcome
}

async fn execute(launch: ExecutionLaunch, state: &ExecutionState) -> Result<()> {
    let ExecutionLaunch {
        runtime,
        endpoint,
        target,
        ready_file,
        scenario_result,
        shutdown,
        scenario_program,
    } = launch;
    let execution = ExecutionId::mint();
    let source = runtime
        .source()
        .ok_or_else(|| anyhow::anyhow!("compiled bundle has no source execution graph"))?;
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

    let (owner, bus) = match ConnectionOwner::open(ConnectionConfig::for_external(
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
        RuntimeExecutionProtocol::open(bus.clone(), source, state.clone(), scenario_program)
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
                RunResources {
                    processes: None,
                    public,
                    owner,
                    router,
                    watchdog,
                    shutdown,
                    router_loss,
                },
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
            RunResources {
                processes: None,
                public,
                owner,
                router,
                watchdog,
                shutdown,
                router_loss,
            },
        )
        .await;
    }
    state.mark_ready();
    let _ = public
        .set_status(phoxal::communication::session::SupervisorState::Ready, None)
        .await;
    let _ = public
        .set_execution_state(&execution.to_string(), PublicExecutionState::Ready)
        .await;
    if let Some(path) = ready_file.as_deref()
        && let Err(error) = publish_readiness(path, execution)
    {
        let error = anyhow::anyhow!("failed to publish supervisor readiness: {error:#}");
        let _ = processes.stop().await;
        mark_execution_failed(&public, execution, &error).await;
        return finish_run(
            Err(error),
            RunResources {
                processes: None,
                public,
                owner,
                router,
                watchdog,
                shutdown,
                router_loss,
            },
        )
        .await;
    }

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
    let outcome = match (scenario_result.as_deref(), protocol.scenario_report().await) {
        (Some(path), Some(report)) => publish_json(path, &report)
            .context("failed to publish scenario execution evidence")
            .and(outcome),
        _ => outcome,
    };
    finish_run(
        outcome,
        RunResources {
            processes: Some(processes),
            public,
            owner,
            router,
            watchdog,
            shutdown,
            router_loss,
        },
    )
    .await
}

fn publish_readiness(path: &Path, execution: ExecutionId) -> Result<()> {
    if path.exists() {
        bail!("readiness path {} already exists", path.display());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("readiness path {} has no parent", path.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("phoxal-ready"),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    let body =
        format!("{{\"schema\":\"phoxal/supervisor-ready/v0\",\"execution\":\"{execution}\"}}\n");
    file.write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| {
        format!(
            "failed to publish readiness from {} to {}",
            temporary.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn publish_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    if path.exists() {
        bail!("result path {} already exists", path.display());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("result path {} has no parent", path.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("phoxal-result"),
        std::process::id()
    ));
    let body = serde_json::to_vec(value).context("failed to encode result JSON")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    file.write_all(&body)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| {
        format!(
            "failed to publish result from {} to {}",
            temporary.display(),
            path.display()
        )
    })?;
    Ok(())
}

async fn start_public_session(
    bus: &Connection,
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
        phoxal::VERSION,
    )?;
    let timeline = state.time_domain().timeline.to_string();
    adapter.install_execution(surface.execution(
        execution.to_string(),
        timeline,
        PublicExecutionState::Preparing as i32,
    )?)?;
    adapter.set_status(
        phoxal::communication::session::SupervisorState::Preparing,
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
            phoxal::communication::session::SupervisorState::Failed,
            Some(detail),
        )
        .await;
    let _ = public
        .set_execution_state(&execution.to_string(), PublicExecutionState::Failed)
        .await;
}

struct RunResources {
    processes: Option<ProcessSupervisor>,
    public: PublicSessionServer,
    owner: ConnectionOwner,
    router: self::router::EmbeddedRouter,
    watchdog: Option<tokio::task::JoinHandle<Result<()>>>,
    shutdown: CancellationToken,
    router_loss: Arc<OnceLock<String>>,
}

async fn finish_run(outcome: Result<()>, resources: RunResources) -> Result<()> {
    let RunResources {
        mut processes,
        public,
        owner,
        router,
        watchdog,
        shutdown,
        router_loss,
    } = resources;
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

/// Compare the controlled simulation's declared quantum (ns) against
/// the scenario program's quantum (us) without truncating either
/// side. Returns a human-readable diagnostic naming the scenario,
/// the program's quantum in both units, and the simulation's
/// quantum in nanoseconds when the two disagree.
fn validate_simulation_quantum(
    scenario_name: &str,
    program_quantum_micros: u32,
    simulation_quantum_ns: u64,
) -> Result<(), String> {
    let required_ns = u128::from(program_quantum_micros)
        .checked_mul(1_000)
        .ok_or_else(|| {
            format!(
                "scenario program `{scenario_name}` declares quantum {program_quantum_micros} micros; \
                 the converted nanosecond value overflows u128 and cannot be compared against \
                 the simulation's quantum"
            )
        })?;
    if u128::from(simulation_quantum_ns) != required_ns {
        return Err(format!(
            "scenario program `{scenario_name}` declares quantum {program_quantum_micros} \
             micros ({required_ns} ns) but the controlled simulation provides \
             {simulation_quantum_ns} ns; refusing to admit mismatched timing"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod quantum_validation_tests {
    use super::validate_simulation_quantum;

    /// 2 ms / 2 ms is the canonical equal case. The previous
    /// integer-truncation comparison accepted the value because both
    /// sides resolved to 2_000 micros; this regression ensures the
    /// nanosecond-aware comparison still accepts it.
    #[test]
    fn equal_quantum_in_nanoseconds_is_accepted() {
        validate_simulation_quantum("scenarios/Equal", 2_000, 2_000_000)
            .expect("equal quantum must validate");
    }

    /// 1 ms / 1 ms is the second canonical equal case. The previous
    /// unsigned-micros comparison admitted it; the new check must
    /// do the same without silently dropping precision.
    #[test]
    fn one_millisecond_quantum_is_accepted() {
        validate_simulation_quantum("scenarios/OneMs", 1_000, 1_000_000)
            .expect("1 ms quantum must validate");
    }

    /// The truncation defect: 2_000_001 ns vs 2_000 us. The old
    /// comparison divided simulation by 1_000 first, producing 2_000
    /// micros on both sides and admitting the bundle. The
    /// nanosecond-aware comparison refuses.
    #[test]
    fn mismatched_quantum_one_ns_over_is_refused() {
        let err = validate_simulation_quantum("scenarios/OneNsOver", 2_000, 2_000_001)
            .expect_err("2_000_001 ns vs 2_000 us must refuse");
        let expected_actual = format!("{}", 2_000_001u64);
        let expected_required = format!("{}", 2_000_000u64);
        assert!(
            err.contains(&expected_actual) && err.contains(&expected_required),
            "diagnostic must name both sides in nanoseconds; got `{err}`"
        );
    }

    /// 1_999_999 ns vs 2_000 us — the reverse truncation defect.
    /// The old comparison would have produced 1_999 micros on the
    /// simulation side, refused; this regression preserves that
    /// refusal with a nanosecond diagnostic.
    #[test]
    fn mismatched_quantum_one_ns_under_is_refused() {
        let err = validate_simulation_quantum("scenarios/OneNsUnder", 2_000, 1_999_999)
            .expect_err("1_999_999 ns vs 2_000 us must refuse");
        let expected_actual = format!("{}", 1_999_999u64);
        let expected_required = format!("{}", 2_000_000u64);
        assert!(
            err.contains(&expected_actual) && err.contains(&expected_required),
            "diagnostic must name both sides in nanoseconds; got `{err}`"
        );
    }

    /// A 10_000_000 ns / 10_000 us case is exactly representable and
    /// must validate, demonstrating that the new comparison does not
    /// introduce a regression for values the previous truncation
    /// handled correctly.
    #[test]
    fn ten_millisecond_quantum_is_accepted() {
        validate_simulation_quantum("scenarios/TenMs", 10_000, 10_000_000)
            .expect("10 ms quantum must validate");
    }
}

async fn verify_router_identity(
    bus: &Connection,
    expected: ExecutionId,
    endpoint: &str,
) -> Result<()> {
    let executions = ConnectionOwner::probe_routers(endpoint).await?;
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
    use super::*;

    #[test]
    fn readiness_is_published_once_after_admission() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("ready.json");
        let execution = ExecutionId::mint();
        publish_readiness(&path, execution)?;
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(value["schema"], "phoxal/supervisor-ready/v0");
        assert_eq!(value["execution"], execution.to_string());
        assert!(publish_readiness(&path, execution).is_err());
        Ok(())
    }
}
