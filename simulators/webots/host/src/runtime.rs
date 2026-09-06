//! Backend-neutral world-session projection over validated native Webots state.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phoxal::identity::ExecutionId;
use phoxal::model::identity::SpawnId;
use phoxal::model::world::{WorldBundle, WorldInstanceId, WorldProgress, WorldProvenance};
use phoxal::supervisor::api::simulation::SimulationEndReason;
use phoxal::version::FrameworkVersion;
use phoxal::world::WorldSessionHandler;
use phoxal::world::api::session::connect::WorldSessionBootstrap;
use phoxal::world::api::session::control::WorldControl;
use phoxal::world::api::session::diagnostics::{ObservedWorldPacing, WorldSessionDiagnostics};
use phoxal::world::api::session::state::WorldSessionState;
use phoxal::world::api::session::{WorldLifecycle, WorldMotion};
use tokio::sync::broadcast;

use crate::evidence::{EvidenceSession, world_checkpoint};
use crate::registration::ProcessIdentity;
use crate::server::HostServer;
use crate::state::{NativeWorldFailure, NativeWorldLifecycle, NativeWorldState};
use phoxal_simulator_webots_shared::protocol::NativeMotion;

const STREAM_CAPACITY: usize = 64;
const PACING_WINDOW_TRANSITIONS: usize = 128;
const DIAGNOSTICS_EMISSION_INTERVAL: Duration = Duration::from_secs(1);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type HostOperation<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Adapter-private implementation of one host-authoritative session.
#[derive(Clone)]
pub struct WorldRuntime {
    bootstrap: WorldSessionBootstrap,
    projection: Arc<Mutex<WorldProjection>>,
    state_updates: broadcast::Sender<WorldSessionState>,
    diagnostics_updates: broadcast::Sender<WorldSessionDiagnostics>,
    native: Arc<HostServer>,
    attachment: Arc<dyn AttachmentOperation>,
    operation: Arc<tokio::sync::Mutex<()>>,
    evidence: Arc<EvidenceSession>,
    process: ProcessIdentity,
}

/// Serialized attachment implementation injected into the public session handler.
pub trait AttachmentOperation: Send + Sync + 'static {
    fn attach<'a>(
        &'a self,
        runtime: &'a WorldRuntime,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> HostOperation<'a, WorldSessionState>;
}

struct DiagnosticsState {
    revision: u64,
    pacing: VecDeque<PacingPoint>,
    last_transition: Option<Instant>,
    last_emission: Option<Instant>,
}

/// All revisioned facts projected from the native world share one owner.
///
/// Keeping state, pacing, and checkpoint throttling together prevents a stale
/// native observation or delayed pacing update from overtaking a newer world
/// transition.
struct WorldProjection {
    state: WorldSessionState,
    diagnostics: DiagnosticsState,
    last_progress_checkpoint: Option<Instant>,
}

#[derive(Clone, Copy)]
struct PacingPoint {
    progress: WorldProgress,
    host: Instant,
}

impl WorldRuntime {
    pub fn new(
        instance: WorldInstanceId,
        bundle: &WorldBundle,
        simulator_version: &str,
        native: Arc<HostServer>,
        attachment: Arc<dyn AttachmentOperation>,
        evidence: Arc<EvidenceSession>,
        process: ProcessIdentity,
    ) -> Result<Self, String> {
        let provenance = WorldProvenance {
            world: bundle.world().id().clone(),
            digest: bundle.digest(),
            random_seed: 0,
            framework: FrameworkVersion::CURRENT,
            adapter: "webots".to_owned(),
            adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
            simulator_version: simulator_version.to_owned(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            time_step_ns: bundle.world().time_step_ns(),
        };
        let state = WorldSessionState {
            revision: 0,
            instance,
            progress: WorldProgress::zero(provenance.time_step_ns)
                .map_err(|error| error.to_string())?,
            provenance,
            lifecycle: WorldLifecycle::Starting,
            members: Vec::new(),
        };
        let (state_updates, _) = broadcast::channel(STREAM_CAPACITY);
        let (diagnostics_updates, _) = broadcast::channel(STREAM_CAPACITY);
        let runtime = Self {
            bootstrap: WorldSessionBootstrap {
                instance,
                framework: FrameworkVersion::CURRENT,
                world: bundle.world().id().clone(),
                digest: bundle.digest(),
            },
            projection: Arc::new(Mutex::new(WorldProjection {
                state: state.clone(),
                diagnostics: DiagnosticsState {
                    revision: 0,
                    pacing: VecDeque::with_capacity(PACING_WINDOW_TRANSITIONS),
                    last_transition: None,
                    last_emission: None,
                },
                last_progress_checkpoint: None,
            })),
            state_updates,
            diagnostics_updates,
            native,
            attachment,
            operation: Arc::new(tokio::sync::Mutex::new(())),
            evidence,
            process,
        };
        runtime.persist_checkpoint(&state)?;
        Ok(runtime)
    }

    /// Publish the first truthful Ready/Paused projection after native bootstrap.
    pub fn mark_ready(&self) -> Result<WorldSessionState, String> {
        self.replace_lifecycle(WorldLifecycle::Ready {
            motion: WorldMotion::Paused,
        })
    }

    /// Retain public stopping intent while native member cleanup converges.
    pub fn mark_stopping(&self) -> Result<WorldSessionState, String> {
        let mut projection = lock(&self.projection);
        self.clear_pacing_locked(&mut projection)?;
        self.replace_lifecycle_locked(&mut projection, WorldLifecycle::Stopping)
    }

    /// Publish a host-classified fatal world outcome before terminal cleanup begins.
    pub fn fail(&self, reason: SimulationEndReason) -> Result<WorldSessionState, String> {
        let mut projection = lock(&self.projection);
        self.clear_pacing_locked(&mut projection)?;
        self.replace_lifecycle_locked(&mut projection, WorldLifecycle::Failed { reason })
    }

    /// Reconcile one latest native snapshot into world progress and fatal state.
    ///
    /// Snapshot acquisition and projection publication share one synchronous
    /// boundary so an older observation cannot overtake a newer projection.
    pub fn reconcile_latest_native(&self) -> Result<NativeWorldState, String> {
        let mut projection = lock(&self.projection);
        let native = self.native.snapshot();
        self.reconcile_observed_native_locked(&mut projection, &native)?;
        Ok(native)
    }

    fn reconcile_observed_native_locked(
        &self,
        projection: &mut WorldProjection,
        native: &NativeWorldState,
    ) -> Result<(), String> {
        match native.lifecycle() {
            NativeWorldLifecycle::Failed(failure) => {
                let reason = failure_reason(failure);
                self.replace_lifecycle_locked(projection, WorldLifecycle::Failed { reason })?;
                return Ok(());
            }
            NativeWorldLifecycle::Stopping => {
                self.replace_lifecycle_locked(projection, WorldLifecycle::Stopping)?;
            }
            NativeWorldLifecycle::Ready { observed, .. } => {
                if !matches!(projection.state.lifecycle, WorldLifecycle::Stopping) {
                    self.replace_lifecycle_locked(
                        projection,
                        WorldLifecycle::Ready {
                            motion: match observed {
                                NativeMotion::Paused => WorldMotion::Paused,
                                NativeMotion::RealTime => WorldMotion::Running,
                            },
                        },
                    )?;
                }
            }
            NativeWorldLifecycle::Starting => {}
        }
        let observed = native.progress();
        let progress = WorldProgress::at(
            observed.completed_step,
            projection.state.provenance.time_step_ns,
        )
        .map_err(|error| error.to_string())?;
        if progress == projection.state.progress {
            return Ok(());
        }
        if progress.completed_step() < projection.state.progress.completed_step() {
            return Err("validated native projection regressed world progress".to_owned());
        }
        projection.state.progress = progress;
        projection.state.revision = next_revision(projection.state.revision)?;
        let projected = projection.state.clone();
        let running = matches!(
            projection.state.lifecycle,
            WorldLifecycle::Ready {
                motion: WorldMotion::Running
            }
        );
        self.persist_progress_checkpoint_locked(projection, &projected)?;
        let _ = self.state_updates.send(projected.clone());
        self.observe_pacing_locked(projection, progress, running)?;
        Ok(())
    }

    pub fn update_state(
        &self,
        change: impl FnOnce(&mut WorldSessionState) -> Result<bool, String>,
    ) -> Result<WorldSessionState, String> {
        let mut projection = lock(&self.projection);
        let mut candidate = projection.state.clone();
        if change(&mut candidate)? {
            candidate
                .members
                .sort_by_key(|member| member.execution.to_string());
            candidate.revision = next_revision(candidate.revision)?;
            candidate.validate().map_err(|error| error.to_string())?;
            projection.state = candidate;
            let projected = projection.state.clone();
            self.persist_checkpoint(&projected)?;
            let _ = self.state_updates.send(projected.clone());
            Ok(projected)
        } else {
            Ok(projection.state.clone())
        }
    }

    async fn apply_control(&self, request: WorldControl) -> Result<WorldSessionState, String> {
        let _operation = self.operation.lock().await;
        match request {
            WorldControl::Pause => {
                let state = self.snapshot();
                if matches!(
                    state.lifecycle,
                    WorldLifecycle::Ready {
                        motion: WorldMotion::Paused
                    }
                ) {
                    return Ok(state);
                }
                if !matches!(state.lifecycle, WorldLifecycle::Ready { .. }) {
                    return Err("only a Ready world can be paused".to_owned());
                }
                self.native
                    .request_motion(NativeMotion::Paused)
                    .map_err(|error| format!("native pause failed: {error:?}"))?;
                self.clear_pacing()?;
                self.await_motion(NativeMotion::Paused).await
            }
            WorldControl::Resume => {
                let state = self.snapshot();
                if matches!(
                    state.lifecycle,
                    WorldLifecycle::Ready {
                        motion: WorldMotion::Running
                    }
                ) {
                    return Ok(state);
                }
                if !matches!(state.lifecycle, WorldLifecycle::Ready { .. }) {
                    return Err("only a Ready world can be resumed".to_owned());
                }
                self.native
                    .request_motion(NativeMotion::RealTime)
                    .map_err(|error| format!("native resume failed: {error:?}"))?;
                self.clear_pacing()?;
                self.await_motion(NativeMotion::RealTime).await
            }
            WorldControl::Stop => {
                let state = self.snapshot();
                if matches!(state.lifecycle, WorldLifecycle::Stopping) {
                    return Ok(state);
                }
                if matches!(state.lifecycle, WorldLifecycle::Failed { .. }) {
                    return Err("a failed world cannot be stopped again".to_owned());
                }
                self.mark_stopping()
            }
        }
    }

    async fn await_motion(&self, expected: NativeMotion) -> Result<WorldSessionState, String> {
        let deadline = tokio::time::Instant::now() + CONTROL_TIMEOUT;
        loop {
            let snapshot = self.reconcile_latest_native()?;
            let native_observed = match snapshot.lifecycle() {
                NativeWorldLifecycle::Ready { observed, .. } => Some(observed),
                NativeWorldLifecycle::Failed(_) => None,
                NativeWorldLifecycle::Starting | NativeWorldLifecycle::Stopping => None,
            };
            let state = self.snapshot();
            if let WorldLifecycle::Failed { reason } = state.lifecycle {
                return Err(format!(
                    "native motion request failed the world: {reason:?}"
                ));
            }
            if native_observed == Some(&expected) && snapshot.robots_observe_motion(expected) {
                return Ok(state);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "Webots did not confirm {expected:?} within {CONTROL_TIMEOUT:?}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    pub(crate) async fn lock_operation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.operation.lock().await
    }

    #[must_use]
    pub fn snapshot(&self) -> WorldSessionState {
        lock(&self.projection).state.clone()
    }

    pub(crate) async fn pause_native_for_operation(&self) -> Result<WorldSessionState, String> {
        self.reconcile_latest_native()?;
        if let WorldLifecycle::Failed { reason } = self.snapshot().lifecycle {
            return Err(format!("native isolation is unavailable: {reason:?}"));
        }
        if matches!(
            self.snapshot().lifecycle,
            WorldLifecycle::Ready {
                motion: WorldMotion::Paused
            }
        ) {
            return Ok(self.snapshot());
        }
        self.native
            .request_motion(NativeMotion::Paused)
            .map_err(|error| format!("native pause failed: {error:?}"))?;
        self.clear_pacing()?;
        self.await_motion(NativeMotion::Paused).await
    }

    pub(crate) async fn restore_native_after_operation(
        &self,
        was_running: bool,
    ) -> Result<WorldSessionState, String> {
        if !was_running {
            return Ok(self.snapshot());
        }
        self.native
            .request_motion(NativeMotion::RealTime)
            .map_err(|error| format!("native resume failed: {error:?}"))?;
        self.clear_pacing()?;
        self.await_motion(NativeMotion::RealTime).await
    }

    fn replace_lifecycle(&self, lifecycle: WorldLifecycle) -> Result<WorldSessionState, String> {
        let mut projection = lock(&self.projection);
        self.replace_lifecycle_locked(&mut projection, lifecycle)
    }

    fn replace_lifecycle_locked(
        &self,
        projection: &mut WorldProjection,
        lifecycle: WorldLifecycle,
    ) -> Result<WorldSessionState, String> {
        if projection.state.lifecycle == lifecycle
            || matches!(projection.state.lifecycle, WorldLifecycle::Failed { .. })
        {
            return Ok(projection.state.clone());
        }
        projection.state.lifecycle = lifecycle;
        projection.state.revision = next_revision(projection.state.revision)?;
        projection
            .state
            .validate()
            .map_err(|error| error.to_string())?;
        let state = projection.state.clone();
        self.persist_checkpoint(&state)?;
        let _ = self.state_updates.send(state.clone());
        Ok(state)
    }

    fn observe_pacing_locked(
        &self,
        projection: &mut WorldProjection,
        progress: WorldProgress,
        running: bool,
    ) -> Result<(), String> {
        let diagnostics = &mut projection.diagnostics;
        let now = Instant::now();
        if !record_pacing(diagnostics, progress, running, now)? {
            return Ok(());
        }
        let projection = project_diagnostics(&diagnostics);
        let _ = self.diagnostics_updates.send(projection);
        Ok(())
    }

    fn clear_pacing(&self) -> Result<(), String> {
        let mut projection = lock(&self.projection);
        self.clear_pacing_locked(&mut projection)
    }

    fn clear_pacing_locked(&self, projection: &mut WorldProjection) -> Result<(), String> {
        let diagnostics = &mut projection.diagnostics;
        clear_pacing_state(diagnostics, Instant::now())?;
        let projection = project_diagnostics(&diagnostics);
        let _ = self.diagnostics_updates.send(projection);
        Ok(())
    }

    fn persist_checkpoint(&self, state: &WorldSessionState) -> Result<(), String> {
        self.evidence
            .write_checkpoint(&world_checkpoint(
                self.process,
                self.evidence.native_process(),
                state.clone(),
            ))
            .map_err(|error| format!("failed to persist world checkpoint: {error:#}"))
    }

    /// Refresh ownership evidence after the separately grouped native process starts.
    pub fn refresh_checkpoint(&self) -> Result<(), String> {
        self.persist_checkpoint(&self.snapshot())
    }

    fn persist_progress_checkpoint_locked(
        &self,
        projection: &mut WorldProjection,
        state: &WorldSessionState,
    ) -> Result<(), String> {
        let now = Instant::now();
        if projection
            .last_progress_checkpoint
            .is_some_and(|last| now.duration_since(last) < DIAGNOSTICS_EMISSION_INTERVAL)
        {
            return Ok(());
        }
        self.persist_checkpoint(state)?;
        projection.last_progress_checkpoint = Some(now);
        Ok(())
    }
}

fn record_pacing(
    diagnostics: &mut DiagnosticsState,
    progress: WorldProgress,
    running: bool,
    now: Instant,
) -> Result<bool, String> {
    if !running {
        diagnostics.pacing.clear();
    } else {
        if diagnostics.pacing.len() == PACING_WINDOW_TRANSITIONS {
            diagnostics.pacing.pop_front();
        }
        diagnostics.pacing.push_back(PacingPoint {
            progress,
            host: now,
        });
    }
    diagnostics.last_transition = Some(now);
    let emit = diagnostics
        .last_emission
        .is_none_or(|last| now.duration_since(last) >= DIAGNOSTICS_EMISSION_INTERVAL);
    if emit {
        diagnostics.last_emission = Some(now);
        diagnostics.revision = next_revision(diagnostics.revision)?;
    }
    Ok(emit)
}

fn clear_pacing_state(diagnostics: &mut DiagnosticsState, now: Instant) -> Result<(), String> {
    diagnostics.pacing.clear();
    diagnostics.last_emission = Some(now);
    diagnostics.revision = next_revision(diagnostics.revision)?;
    Ok(())
}

impl WorldSessionHandler for WorldRuntime {
    fn bootstrap(&self) -> WorldSessionBootstrap {
        self.bootstrap.clone()
    }

    fn state(&self) -> WorldSessionState {
        self.snapshot()
    }

    fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState> {
        self.state_updates.subscribe()
    }

    fn diagnostics(&self) -> WorldSessionDiagnostics {
        project_diagnostics(&lock(&self.projection).diagnostics)
    }

    fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics> {
        self.diagnostics_updates.subscribe()
    }

    fn control(&self, request: WorldControl) -> HostOperation<'_, WorldSessionState> {
        Box::pin(async move { self.apply_control(request).await })
    }

    fn attach(
        &self,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> HostOperation<'_, WorldSessionState> {
        self.attachment
            .attach(self, execution, supervisor_endpoint, spawn)
    }
}

fn project_diagnostics(state: &DiagnosticsState) -> WorldSessionDiagnostics {
    let pacing = match (state.pacing.front(), state.pacing.back()) {
        (Some(first), Some(last)) if state.pacing.len() >= 2 => {
            let world_elapsed_ns = last
                .progress
                .elapsed_ns()
                .saturating_sub(first.progress.elapsed_ns());
            let host_elapsed_ns =
                u64::try_from(last.host.duration_since(first.host).as_nanos()).unwrap_or(u64::MAX);
            let completed_transitions = last
                .progress
                .completed_step()
                .saturating_sub(first.progress.completed_step());
            let observed = ObservedWorldPacing {
                world_elapsed_ns,
                host_elapsed_ns,
                completed_transitions,
            };
            observed.is_valid().then_some(observed)
        }
        _ => None,
    };
    WorldSessionDiagnostics {
        revision: state.revision,
        pacing,
        last_transition_age_ns: state
            .last_transition
            .map(|instant| u64::try_from(instant.elapsed().as_nanos()).unwrap_or(u64::MAX)),
    }
}

const fn failure_reason(failure: &NativeWorldFailure) -> SimulationEndReason {
    match failure {
        NativeWorldFailure::UnsupportedMode(_) => SimulationEndReason::UnsupportedNativeMode,
        NativeWorldFailure::InvalidProgress { .. } => SimulationEndReason::InvalidProgress,
        NativeWorldFailure::WorldControllerLost => SimulationEndReason::WorldControllerLost,
        NativeWorldFailure::RobotControllerLost { .. } => SimulationEndReason::ControllerLost,
        NativeWorldFailure::Controller(_)
        | NativeWorldFailure::DuplicateWorldController
        | NativeWorldFailure::DuplicateRobot { .. }
        | NativeWorldFailure::IncompatibleController { .. }
        | NativeWorldFailure::InvalidTimeStep
        | NativeWorldFailure::Protocol(_) => SimulationEndReason::ProtocolViolation,
    }
}

fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "world-session revision exhausted".to_owned())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_samples_every_transition_but_publishes_at_most_once_per_second() {
        let start = Instant::now();
        let mut diagnostics = DiagnosticsState {
            revision: 0,
            pacing: VecDeque::new(),
            last_transition: None,
            last_emission: None,
        };
        assert!(
            record_pacing(
                &mut diagnostics,
                WorldProgress::at(1, 10).expect("progress"),
                true,
                start,
            )
            .expect("first pacing sample")
        );
        assert_eq!(diagnostics.revision, 1);
        assert!(
            !record_pacing(
                &mut diagnostics,
                WorldProgress::at(2, 10).expect("progress"),
                true,
                start + Duration::from_millis(500),
            )
            .expect("second pacing sample")
        );
        assert_eq!(diagnostics.pacing.len(), 2);
        assert_eq!(diagnostics.revision, 1);
        assert!(
            record_pacing(
                &mut diagnostics,
                WorldProgress::at(3, 10).expect("progress"),
                true,
                start + DIAGNOSTICS_EMISSION_INTERVAL,
            )
            .expect("third pacing sample")
        );
        assert_eq!(diagnostics.pacing.len(), 3);
        assert_eq!(diagnostics.revision, 2);
    }

    #[test]
    fn pause_clears_only_the_window_and_publishes_a_revision() {
        let transition = Instant::now();
        let mut diagnostics = DiagnosticsState {
            revision: 7,
            pacing: VecDeque::from([PacingPoint {
                progress: WorldProgress::at(4, 10).expect("progress"),
                host: transition,
            }]),
            last_transition: Some(transition),
            last_emission: Some(transition),
        };
        clear_pacing_state(&mut diagnostics, transition + Duration::from_millis(1))
            .expect("pause pacing clear");
        assert!(diagnostics.pacing.is_empty());
        assert_eq!(diagnostics.last_transition, Some(transition));
        assert_eq!(diagnostics.revision, 8);
        let projection = project_diagnostics(&diagnostics);
        assert_eq!(projection.revision, 8);
        assert!(projection.pacing.is_none());
        assert!(projection.last_transition_age_ns.is_some());
    }

    #[test]
    fn below_one_pacing_is_retained_as_observation_without_becoming_a_target() {
        let start = Instant::now();
        let mut diagnostics = DiagnosticsState {
            revision: 0,
            pacing: VecDeque::new(),
            last_transition: None,
            last_emission: None,
        };
        record_pacing(
            &mut diagnostics,
            WorldProgress::at(1, 10).expect("first progress"),
            true,
            start,
        )
        .expect("first pacing sample");
        record_pacing(
            &mut diagnostics,
            WorldProgress::at(2, 10).expect("second progress"),
            true,
            start + Duration::from_nanos(40),
        )
        .expect("second pacing sample");

        let pacing = project_diagnostics(&diagnostics)
            .pacing
            .expect("two transitions form one observation");
        assert_eq!(pacing.world_elapsed_ns, 10);
        assert_eq!(pacing.host_elapsed_ns, 40);
        assert_eq!(pacing.completed_transitions, 1);
        assert!(pacing.world_elapsed_ns < pacing.host_elapsed_ns);
    }
}
