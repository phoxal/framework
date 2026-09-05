//! Long-lived Webots world-session host.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use phoxal::model::world::{WorldBundle, WorldInstanceId};
use phoxal::supervisor::api::simulation::SimulationEndReason;
use phoxal::world::WorldSessionServer;
use phoxal::world::api::session::document::{
    TerminalCleanup, TerminalFailure, TerminalOutcome, TerminalRetention,
};
use phoxal::world::api::session::{WorldLifecycle, WorldMember, WorldMemberPhase};
use phoxal_simulator_webots_host::attachment::WebotsAttachments;
use phoxal_simulator_webots_host::evidence::{EvidenceSession, world_terminal_summary};
use phoxal_simulator_webots_host::generation::{ControllerExecutables, stage_project};
use phoxal_simulator_webots_host::lifecycle::{
    LogCaptureOutcome, NativeProcessIdentity, WebotsInstallation, WebotsProcess,
};
use phoxal_simulator_webots_host::registration::{
    EVIDENCE_DIRECTORY_ENV, LOG_BYTE_LIMIT_ENV, REGISTRY_DIRECTORY_ENV, RegistrationGuard,
    current_process_identity,
};
use phoxal_simulator_webots_host::runtime::WorldRuntime;
use phoxal_simulator_webots_host::server::HostServer;
use phoxal_simulator_webots_host::state::{NativeWorldFailure, NativeWorldLifecycle};
use phoxal_simulator_webots_host::{ROBOT_CONTROLLER_PACKAGE, WORLD_CONTROLLER_PACKAGE};
use tracing_subscriber::EnvFilter;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
const WORLD_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const RECONCILE_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Canonical compiled WorldBundle directory.
    #[arg(long, value_name = "PATH")]
    world_bundle: PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let log_byte_limit = match required_log_limit() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("webots host configuration failed: {error:#}");
            std::process::exit(2);
        }
    };
    let host_log_limit = (log_byte_limit / 2).max(1);
    let host_log = BoundedStderr::new(host_log_limit);
    let host_log_observer = host_log.clone();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(move || host_log.clone())
        .init();
    if let Err(error) = run(args, log_byte_limit, host_log_observer).await {
        tracing::error!(error = %format!("{error:#}"), "Webots world host failed");
        std::process::exit(1);
    }
}

async fn run(args: Args, log_byte_limit: u64, host_log: BoundedStderr) -> Result<()> {
    let bundle = WorldBundle::open(&args.world_bundle).with_context(|| {
        format!(
            "failed to open world bundle {}",
            args.world_bundle.display()
        )
    })?;
    let instance = WorldInstanceId::mint();
    let evidence_root = required_path(EVIDENCE_DIRECTORY_ENV)?;
    let registry_root = required_path(REGISTRY_DIRECTORY_ENV)?;
    let evidence = Arc::new(EvidenceSession::create(
        &evidence_root,
        instance,
        &bundle,
        log_byte_limit,
    )?);
    let process = current_process_identity()?;
    let installation = WebotsInstallation::discover()?;
    let native = Arc::new(HostServer::bind()?);
    let executable_directory = std::env::current_exe()?
        .parent()
        .context("host executable has no containing directory")?
        .to_path_buf();
    let controllers = ControllerExecutables {
        world: executable_directory.join(WORLD_CONTROLLER_PACKAGE),
        robot: executable_directory.join(ROBOT_CONTROLLER_PACKAGE),
    };
    let staging = tempfile::Builder::new()
        .prefix("phoxal-webots-")
        .tempdir()
        .context("failed to create the native Webots staging directory")?;
    let project_root = staging.path().join("project");
    let project = stage_project(&bundle, &project_root, native.endpoint(), &controllers)?;
    let attachments = Arc::new(WebotsAttachments::new(
        instance,
        bundle.world().clone(),
        project_root,
        Arc::clone(&native),
        Arc::clone(&evidence),
    ));
    let runtime = Arc::new(
        WorldRuntime::new(
            instance,
            &bundle,
            installation.version(),
            Arc::clone(&native),
            Arc::clone(&attachments)
                as Arc<dyn phoxal_simulator_webots_host::runtime::AttachmentOperation>,
            Arc::clone(&evidence),
            process,
        )
        .map_err(anyhow::Error::msg)?,
    );
    let webots_limit = log_byte_limit.saturating_sub(log_byte_limit / 2).max(1);
    let mut webots = WebotsProcess::launch(
        &installation,
        project.world(),
        &evidence.webots_log(),
        webots_limit,
        false,
    )?;
    evidence.set_native_process(webots.identity()?);
    runtime.refresh_checkpoint().map_err(anyhow::Error::msg)?;

    let mut public = None;
    let mut registration = None;
    let live_result = serve_live(
        &bundle,
        instance,
        &registry_root,
        &runtime,
        &attachments,
        &native,
        &mut webots,
        process,
        &mut public,
        &mut registration,
    )
    .await;
    let state_before_cleanup = runtime.snapshot();
    let native_before_cleanup = native.snapshot();
    let members_before_cleanup = state_before_cleanup.members.clone();
    let terminal_reason = match state_before_cleanup.lifecycle {
        WorldLifecycle::Failed { reason } => Some(reason),
        WorldLifecycle::Starting | WorldLifecycle::Ready { .. } | WorldLifecycle::Stopping => None,
    };
    let end_reason = match state_before_cleanup.lifecycle {
        WorldLifecycle::Failed { reason } => reason,
        _ => SimulationEndReason::WorldStopped,
    };
    let member_cleanup_detail = attachments
        .end_all(&runtime, end_reason)
        .await
        .err()
        .map(|error| format!("{error:#}"));
    native.stop_world();
    let controller_cleanup_detail = await_world_controller_stop(&native, &mut webots)
        .await
        .err()
        .map(|error| format!("{error:#}"));
    let stopped = webots.stop().await;
    let (webots_log, native_cleanup_detail) = match stopped {
        Ok(outcome) => (outcome, None),
        Err(error) => (
            LogCaptureOutcome {
                bytes: 0,
                truncated: true,
            },
            Some(format!("{error:#}")),
        ),
    };
    let public_cleanup_detail = match public.take() {
        Some(public) => public
            .close()
            .await
            .err()
            .map(|error| format!("failed to close public world-session endpoint: {error}")),
        None => None,
    };
    let cleanup_detail = [
        member_cleanup_detail.clone(),
        controller_cleanup_detail.clone(),
        native_cleanup_detail.clone(),
        public_cleanup_detail.clone(),
    ]
    .into_iter()
    .flatten()
    .reduce(|left, right| format!("{left}; {right}"));
    let cleanup_failure_reason = cleanup_detail.as_ref().map(|_| {
        if member_cleanup_detail.is_some() {
            SimulationEndReason::RemovalFailed
        } else if controller_cleanup_detail.is_some() {
            SimulationEndReason::WorldControllerLost
        } else if native_cleanup_detail.is_some() {
            SimulationEndReason::SimulatorLost
        } else {
            SimulationEndReason::ProtocolViolation
        }
    });
    let failure_reason = terminal_reason.or(cleanup_failure_reason);
    let failing = failing_identity(
        failure_reason,
        native_before_cleanup.lifecycle(),
        &members_before_cleanup,
        evidence.native_process().as_ref(),
    );
    let outcome = terminal_outcome(
        live_result.as_ref().err().map(|error| format!("{error:#}")),
        failure_reason,
        cleanup_detail.clone(),
    );
    let mut truncated = Vec::new();
    if host_log.truncated() {
        truncated.push("host.log".to_owned());
    }
    if webots_log.truncated {
        truncated.push("webots.log".to_owned());
    }
    evidence.write_summary(&world_terminal_summary(
        instance,
        state_before_cleanup.provenance,
        outcome,
        runtime.snapshot().progress,
        members_before_cleanup,
        evidence.member_evidence()?,
        failing,
        TerminalCleanup {
            complete: cleanup_detail.is_none(),
            detail: cleanup_detail.clone(),
        },
        TerminalRetention {
            log_byte_limit,
            truncated,
        },
    ))?;
    // Keep the owner-held lease discoverable until every native/member authority has converged
    // and terminal evidence is atomically durable. Registration is removed last.
    drop(registration.take());
    match (live_result, cleanup_detail) {
        (Ok(()), None) => Ok(()),
        (Ok(()), Some(detail)) => bail!("terminal cleanup failed: {detail}"),
        (Err(error), _) => Err(error),
    }
}

fn terminal_outcome(
    live_error: Option<String>,
    failure_reason: Option<SimulationEndReason>,
    cleanup_detail: Option<String>,
) -> TerminalOutcome {
    match (live_error, failure_reason, cleanup_detail) {
        (None, None, None) => TerminalOutcome::Stopped {
            reason: SimulationEndReason::WorldStopped,
        },
        (live_error, Some(reason), cleanup_detail) => TerminalOutcome::Failed {
            reason,
            detail: [live_error, cleanup_detail]
                .into_iter()
                .flatten()
                .reduce(|left, right| format!("{left}; {right}"))
                .unwrap_or_else(|| format!("world session failed with {reason:?}")),
        },
        (Some(detail), None, None) => TerminalOutcome::Failed {
            reason: SimulationEndReason::ProtocolViolation,
            detail,
        },
        (live_error, None, Some(cleanup)) => TerminalOutcome::Failed {
            reason: SimulationEndReason::ProtocolViolation,
            detail: live_error.map_or(cleanup.clone(), |live| format!("{live}; {cleanup}")),
        },
    }
}

async fn await_world_controller_stop(
    native: &HostServer,
    webots: &mut WebotsProcess,
) -> Result<()> {
    if !native.has_world_controller() {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + WORLD_STOP_TIMEOUT;
    loop {
        if native.world_is_stopped() {
            return Ok(());
        }
        if let Some(status) = webots.exited()? {
            bail!("Webots exited with {status} before the world controller acknowledged stop");
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("world controller did not acknowledge stop within {WORLD_STOP_TIMEOUT:?}");
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the live loop coordinates the complete set of independently owned host authorities"
)]
async fn serve_live(
    bundle: &WorldBundle,
    instance: WorldInstanceId,
    registry_root: &Path,
    runtime: &Arc<WorldRuntime>,
    attachments: &Arc<WebotsAttachments>,
    native: &Arc<HostServer>,
    webots: &mut WebotsProcess,
    process: phoxal_simulator_webots_host::registration::ProcessIdentity,
    public_slot: &mut Option<WorldSessionServer>,
    registration_slot: &mut Option<RegistrationGuard>,
) -> Result<()> {
    // The CLI rolls back a transaction-owned host with SIGTERM. Install its
    // handler before publishing readiness so rollback uses the same cleanup
    // path and retained evidence as an explicit world stop.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to observe world-host termination")?;
    let deadline = tokio::time::Instant::now() + BOOTSTRAP_TIMEOUT;
    loop {
        if let Some(status) = webots.exited()? {
            return fail_for_webots_exit(
                runtime,
                format!("Webots exited during bootstrap with {status}"),
            );
        }
        native.enforce_liveness();
        let snapshot = native.snapshot();
        match snapshot.lifecycle() {
            NativeWorldLifecycle::Ready { .. } => break,
            NativeWorldLifecycle::Failed(reason) => {
                runtime
                    .reconcile_native(&snapshot)
                    .map_err(anyhow::Error::msg)?;
                bail!("native Webots bootstrap failed: {reason:?}");
            }
            NativeWorldLifecycle::Starting | NativeWorldLifecycle::Stopping => {}
        }
        if tokio::time::Instant::now() >= deadline {
            runtime
                .fail(SimulationEndReason::WorldControllerLost)
                .map_err(anyhow::Error::msg)?;
            bail!("Webots world controller did not become ready within {BOOTSTRAP_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    runtime.mark_ready().map_err(anyhow::Error::msg)?;
    let public = WorldSessionServer::bind(Arc::clone(runtime))
        .await
        .context("failed to bind the public world-session endpoint")?;
    let registration = RegistrationGuard::create(
        registry_root,
        instance,
        public.endpoint().to_owned(),
        bundle,
        process,
    )?;
    let endpoint = public.endpoint().to_owned();
    *public_slot = Some(public);
    *registration_slot = Some(registration);
    println!("{instance}");
    std::io::stdout()
        .flush()
        .context("failed to publish the world ready line")?;
    tracing::info!(
        %instance,
        world = %bundle.world().id(),
        digest = %bundle.digest(),
        endpoint,
        "native Webots world is ready and paused"
    );

    let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to wait for the stop signal")?;
                runtime.mark_stopping().map_err(anyhow::Error::msg)?;
            }
            _ = terminate.recv() => {
                runtime.mark_stopping().map_err(anyhow::Error::msg)?;
            }
            _ = interval.tick() => {}
        }
        if let Some(status) = webots.exited()? {
            return fail_for_webots_exit(
                runtime,
                format!("Webots exited unexpectedly with {status}"),
            );
        }
        native.enforce_liveness();
        let snapshot = native.snapshot();
        runtime
            .reconcile_native(&snapshot)
            .map_err(anyhow::Error::msg)?;
        match runtime.snapshot().lifecycle {
            WorldLifecycle::Stopping => break,
            WorldLifecycle::Failed { reason } => {
                bail!(
                    "native world failed: {reason:?}; {:?}",
                    snapshot.lifecycle()
                );
            }
            WorldLifecycle::Starting | WorldLifecycle::Ready { .. } => {}
        }
        attachments.reconcile_removals(runtime).await?;
    }
    Ok(())
}

fn fail_for_webots_exit(runtime: &WorldRuntime, detail: String) -> Result<()> {
    if let Err(error) = runtime.fail(SimulationEndReason::SimulatorLost) {
        bail!("{detail}; failed to publish SimulatorLost: {error}");
    }
    bail!("{detail}")
}

fn failing_identity(
    reason: Option<SimulationEndReason>,
    native_lifecycle: &NativeWorldLifecycle,
    members: &[WorldMember],
    native_process: Option<&NativeProcessIdentity>,
) -> TerminalFailure {
    let process = if reason == Some(SimulationEndReason::SimulatorLost) {
        native_process.map(|identity| identity.process)
    } else {
        None
    };
    let native_execution = match native_lifecycle {
        NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost { execution }) => {
            Some(execution.as_str())
        }
        NativeWorldLifecycle::Starting
        | NativeWorldLifecycle::Ready { .. }
        | NativeWorldLifecycle::Stopping
        | NativeWorldLifecycle::Failed(_) => None,
    };
    let producer = native_execution
        .and_then(|execution| {
            members
                .iter()
                .find(|member| member.execution.to_string() == execution)
                .map(|member| member.controller)
        })
        .or_else(|| match reason {
            Some(SimulationEndReason::MutationFailed) => {
                unique_member_producer(members, WorldMemberPhase::Preparing)
            }
            Some(SimulationEndReason::RemovalFailed) => {
                unique_member_producer(members, WorldMemberPhase::Removing)
            }
            Some(
                SimulationEndReason::WorldStopped
                | SimulationEndReason::HostLost
                | SimulationEndReason::SimulatorLost
                | SimulationEndReason::WorldControllerLost
                | SimulationEndReason::ControllerLost
                | SimulationEndReason::UnsupportedNativeMode
                | SimulationEndReason::InvalidProgress
                | SimulationEndReason::ProtocolViolation,
            )
            | None => None,
        });
    TerminalFailure { process, producer }
}

fn unique_member_producer(
    members: &[WorldMember],
    phase: WorldMemberPhase,
) -> Option<phoxal::identity::ProducerId> {
    let mut candidates = members
        .iter()
        .filter(|member| member.phase == phase)
        .map(|member| member.controller);
    let producer = candidates.next()?;
    candidates.next().is_none().then_some(producer)
}

fn required_path(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("required environment variable {name} is missing"))
}

fn required_log_limit() -> Result<u64> {
    let value = std::env::var(LOG_BYTE_LIMIT_ENV).with_context(|| {
        format!("required environment variable {LOG_BYTE_LIMIT_ENV} is missing")
    })?;
    let value = value
        .parse::<u64>()
        .with_context(|| format!("{LOG_BYTE_LIMIT_ENV} must contain decimal bytes"))?;
    ensure!(value >= 2, "{LOG_BYTE_LIMIT_ENV} must be at least 2 bytes");
    Ok(value)
}

#[derive(Clone)]
struct BoundedStderr {
    state: Arc<Mutex<BoundedStderrState>>,
}

struct BoundedStderrState {
    limit: u64,
    written: u64,
    truncated: bool,
}

impl BoundedStderr {
    fn new(limit: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(BoundedStderrState {
                limit,
                written: 0,
                truncated: false,
            })),
        }
    }

    fn truncated(&self) -> bool {
        lock(&self.state).truncated
    }
}

impl std::io::Write for BoundedStderr {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = lock(&self.state);
        let remaining = state.limit.saturating_sub(state.written);
        let retained = usize::try_from(remaining.min(bytes.len() as u64)).unwrap_or(bytes.len());
        if retained > 0 {
            std::io::stderr().write_all(&bytes[..retained])?;
            state.written = state.written.saturating_add(retained as u64);
        }
        if retained < bytes.len() {
            state.truncated = true;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::bus::RobotInstant;
    use phoxal::identity::{ExecutionId, ProducerId, TimelineId};
    use phoxal::model::identity::{RobotId, SpawnId};
    use phoxal::model::world::{LiveAttachmentBoundary, WorldProgress};
    use phoxal_simulator_webots_host::registration::ProcessIdentity;

    fn member(execution: &str, producer: u128, phase: WorldMemberPhase) -> WorldMember {
        WorldMember {
            execution: ExecutionId::parse(execution).expect("canonical execution"),
            robot: RobotId::new("robot").expect("robot id"),
            controller: ProducerId::try_from(producer).expect("producer id"),
            phase,
            attached_at: LiveAttachmentBoundary {
                world: WorldProgress::zero(12_000_000).expect("world progress"),
                execution: RobotInstant::new(TimelineId::from_raw(1).expect("timeline"), 0),
            },
            spawn: SpawnId::new("spawn").expect("spawn id"),
            initial_pose: serde_json::from_value(serde_json::json!({
                "xyz": [0.0, 0.0, 0.0],
                "rpy": [0.0, 0.0, 0.0]
            }))
            .expect("pose"),
        }
    }

    #[test]
    fn hard_controller_loss_names_the_exact_pre_cleanup_member() {
        let first = member(
            "10000000000000000000000000000001",
            0x3000_0000_0000_0000_0000_0000_0000_0003,
            WorldMemberPhase::Active,
        );
        let second = member(
            "20000000000000000000000000000002",
            0x4000_0000_0000_0000_0000_0000_0000_0004,
            WorldMemberPhase::Active,
        );
        let lifecycle = NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost {
            execution: second.execution.to_string(),
        });

        let failing = failing_identity(
            Some(SimulationEndReason::ControllerLost),
            &lifecycle,
            &[first, second.clone()],
            None,
        );

        assert_eq!(failing.producer, Some(second.controller));
        assert_eq!(failing.process, None);
    }

    #[test]
    fn simulator_loss_names_the_owned_native_process() {
        let identity = NativeProcessIdentity {
            process: ProcessIdentity {
                pid: 123,
                started_at_unix_s: 456,
            },
            executable: PathBuf::from("/Applications/Webots.app/Contents/MacOS/webots"),
            process_group: Some(123),
        };

        let failing = failing_identity(
            Some(SimulationEndReason::SimulatorLost),
            &NativeWorldLifecycle::Starting,
            &[],
            Some(&identity),
        );

        assert_eq!(failing.process, Some(identity.process));
        assert_eq!(failing.producer, None);
    }

    #[test]
    fn removal_failure_names_only_an_unambiguous_removing_member() {
        let active = member(
            "10000000000000000000000000000001",
            0x3000_0000_0000_0000_0000_0000_0000_0003,
            WorldMemberPhase::Active,
        );
        let removing = member(
            "20000000000000000000000000000002",
            0x4000_0000_0000_0000_0000_0000_0000_0004,
            WorldMemberPhase::Removing,
        );

        let failing = failing_identity(
            Some(SimulationEndReason::RemovalFailed),
            &NativeWorldLifecycle::Starting,
            &[active, removing.clone()],
            None,
        );

        assert_eq!(failing.producer, Some(removing.controller));
    }

    #[test]
    fn terminal_cleanup_failure_cannot_be_reported_as_an_orderly_stop() {
        let outcome = terminal_outcome(
            None,
            Some(SimulationEndReason::RemovalFailed),
            Some("Robot controller did not confirm parked".to_owned()),
        );
        assert!(matches!(
            outcome,
            TerminalOutcome::Failed {
                reason: SimulationEndReason::RemovalFailed,
                ref detail,
            } if detail.contains("did not confirm parked")
        ));
    }
}
