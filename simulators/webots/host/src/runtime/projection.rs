use super::*;

/// Adapter-private implementation of one host-authoritative session.
#[derive(Clone)]
pub struct WorldRuntime {
    pub(super) bootstrap: WorldSessionBootstrap,
    pub(super) projection: Arc<Mutex<WorldProjection>>,
    pub(super) state_updates: broadcast::Sender<WorldSessionState>,
    pub(super) diagnostics_updates: broadcast::Sender<WorldSessionDiagnostics>,
    pub(super) native: Arc<HostServer>,
    pub(super) operation: Arc<tokio::sync::Mutex<()>>,
    pub(super) evidence: Arc<EvidenceSession>,
    pub(super) checkpoints: Arc<CheckpointWriter>,
    pub(super) process: ProcessIdentity,
}

/// All revisioned facts projected from the native world share one owner.
///
/// Keeping state, pacing, and checkpoint throttling together prevents a stale
/// native observation or delayed pacing update from overtaking a newer world
/// transition.
pub(super) struct WorldProjection {
    pub(super) state: WorldSessionState,
    pub(super) diagnostics: DiagnosticsState,
    pub(super) last_progress_checkpoint: Option<Instant>,
}

impl WorldRuntime {
    pub fn new(
        instance: WorldInstanceId,
        bundle: &WorldBundle,
        simulator_version: &str,
        native: Arc<HostServer>,
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
            operation: Arc::new(tokio::sync::Mutex::new(())),
            checkpoints: Arc::new(CheckpointWriter::new(Arc::clone(&evidence))?),
            evidence,
            process,
        };
        runtime.persist_checkpoint(&state)?;
        runtime.checkpoints.flush()?;
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
        let state = {
            let mut projection = lock(&self.projection);
            self.clear_pacing_locked(&mut projection)?;
            self.replace_lifecycle_locked(&mut projection, WorldLifecycle::Stopping)?
        };
        self.checkpoints.flush()?;
        Ok(state)
    }

    /// Publish a host-classified fatal world outcome before terminal cleanup begins.
    pub fn fail(&self, reason: SimulationEndReason) -> Result<WorldSessionState, String> {
        let state = {
            let mut projection = lock(&self.projection);
            self.clear_pacing_locked(&mut projection)?;
            self.replace_lifecycle_locked(&mut projection, WorldLifecycle::Failed { reason })?
        };
        self.checkpoints.flush()?;
        Ok(state)
    }

    /// Reconcile one latest native snapshot into world progress and fatal state.
    ///
    /// Snapshot acquisition and projection publication share one synchronous
    /// boundary so an older observation cannot overtake a newer projection.
    pub fn reconcile_latest_native(&self) -> Result<NativeWorldState, String> {
        let native = self.native.snapshot();
        {
            let mut projection = lock(&self.projection);
            self.reconcile_observed_native_locked(&mut projection, &native)?;
        }
        self.checkpoints.flush()?;
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

    fn update_state(
        &self,
        change: impl FnOnce(&mut WorldSessionState) -> Result<bool, String>,
    ) -> Result<WorldSessionState, String> {
        let state = {
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
                projected
            } else {
                projection.state.clone()
            }
        };
        self.checkpoints.flush()?;
        Ok(state)
    }

    pub fn prepare_member(
        &self,
        member: phoxal::world::api::session::WorldMember,
    ) -> Result<WorldSessionState, String> {
        self.update_state(|state| {
            if state
                .members
                .iter()
                .any(|existing| existing.execution == member.execution)
            {
                return Err(format!("execution {} joined twice", member.execution));
            }
            if state
                .members
                .iter()
                .any(|existing| existing.spawn == member.spawn)
            {
                return Err(format!("spawn point '{}' became occupied", member.spawn));
            }
            state.members.push(member);
            Ok(true)
        })
    }

    pub fn activate_member(
        &self,
        member: phoxal::world::api::session::WorldMember,
    ) -> Result<WorldSessionState, String> {
        self.update_state(|state| {
            if let Some(existing) = state
                .members
                .iter_mut()
                .find(|existing| existing.execution == member.execution)
            {
                *existing = member;
            } else {
                if state
                    .members
                    .iter()
                    .any(|existing| existing.spawn == member.spawn)
                {
                    return Err(format!("spawn point '{}' became occupied", member.spawn));
                }
                state.members.push(member);
            }
            Ok(true)
        })
    }

    pub fn mark_member_removing(
        &self,
        execution: ExecutionId,
    ) -> Result<WorldSessionState, String> {
        self.update_state(|state| {
            let Some(member) = state
                .members
                .iter_mut()
                .find(|member| member.execution == execution)
            else {
                return Ok(false);
            };
            member.phase = phoxal::world::api::session::WorldMemberPhase::Removing;
            Ok(true)
        })
    }

    pub fn complete_member_removal(
        &self,
        execution: ExecutionId,
    ) -> Result<WorldSessionState, String> {
        self.update_state(|state| {
            let before = state.members.len();
            state.members.retain(|member| member.execution != execution);
            Ok(state.members.len() != before)
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> WorldSessionState {
        lock(&self.projection).state.clone()
    }

    pub(crate) fn bootstrap(&self) -> WorldSessionBootstrap {
        self.bootstrap.clone()
    }

    pub(crate) fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState> {
        self.state_updates.subscribe()
    }

    pub(crate) fn diagnostics(&self) -> WorldSessionDiagnostics {
        project_diagnostics(&lock(&self.projection).diagnostics)
    }

    pub(crate) fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics> {
        self.diagnostics_updates.subscribe()
    }

    fn replace_lifecycle(&self, lifecycle: WorldLifecycle) -> Result<WorldSessionState, String> {
        let state = {
            let mut projection = lock(&self.projection);
            self.replace_lifecycle_locked(&mut projection, lifecycle)?
        };
        self.checkpoints.flush()?;
        Ok(state)
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
        let projection = project_diagnostics(diagnostics);
        let _ = self.diagnostics_updates.send(projection);
        Ok(())
    }

    pub(super) fn clear_pacing(&self) -> Result<(), String> {
        let mut projection = lock(&self.projection);
        self.clear_pacing_locked(&mut projection)
    }

    fn clear_pacing_locked(&self, projection: &mut WorldProjection) -> Result<(), String> {
        let diagnostics = &mut projection.diagnostics;
        clear_pacing_state(diagnostics, Instant::now())?;
        let projection = project_diagnostics(diagnostics);
        let _ = self.diagnostics_updates.send(projection);
        Ok(())
    }

    fn persist_checkpoint(&self, state: &WorldSessionState) -> Result<(), String> {
        self.checkpoints.submit(world_checkpoint(
            self.process,
            self.evidence.native_process(),
            state.clone(),
        ))
    }

    /// Refresh ownership evidence after the separately grouped native process starts.
    pub fn refresh_checkpoint(&self) -> Result<(), String> {
        self.persist_checkpoint(&self.snapshot())?;
        self.checkpoints.flush()
    }

    /// Stop the checkpoint owner before terminal summary publication.
    pub fn finish_evidence_writer(&self) -> Result<(), String> {
        self.checkpoints.finish()
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

pub(super) fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "world-session revision exhausted".to_owned())
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
