//! Serialized robot admission, native import, and supervisor commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use phoxal::identity::ExecutionId;
use phoxal::model::asset::AssetId;
use phoxal::model::identity::SpawnId;
use phoxal::model::world::{World, WorldInstanceId};
use phoxal::simulator::{SimulationHostConnectOptions, SimulationHostSession};
use phoxal::supervisor::api::simulation::SimulationEndReason;
use phoxal::supervisor::api::simulation::attach::AttachRequest;
use phoxal::world::api::session::WorldMember;
use phoxal::world::api::session::WorldMemberPhase;
use phoxal::world::api::session::state::WorldSessionState;
use phoxal::world::api::session::{WorldMemberCleanup, WorldMemberEndReason, WorldMemberTerminal};
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use crate::evidence::{EvidenceSession, world_member_evidence};
use crate::generation::stage_decoded_images;
use crate::glb::DecodedMesh;
use crate::plan::RobotSimulationPlan;
use crate::robot_generation::{render_robot, robot_definition};
use crate::runtime::{AttachmentOperation, HostOperation, WorldRuntime};
use crate::server::HostServer;
use crate::state::{NativeRobotFailure, NativeWorldLifecycle};

const CONTROLLER_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Concrete attachment authority retained by one world session.
#[derive(Clone)]
pub struct WebotsAttachments {
    instance: WorldInstanceId,
    world: World,
    project_root: PathBuf,
    native: Arc<HostServer>,
    evidence: Arc<EvidenceSession>,
    sessions: Arc<Mutex<BTreeMap<String, AttachedSession>>>,
    workers: Arc<Mutex<JoinSet<()>>>,
    cancellations: Arc<std::sync::Mutex<Vec<Weak<AtomicBool>>>>,
}

#[derive(Clone)]
struct OperationCancellation(Arc<AtomicBool>);

impl OperationCancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn check(&self) -> Result<()> {
        ensure!(
            !self.0.load(Ordering::Acquire),
            "world attachment operation was cancelled"
        );
        Ok(())
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

struct CancelOnDrop {
    cancellation: OperationCancellation,
    armed: bool,
}

#[derive(Default)]
struct ImportOwnership {
    attempted: bool,
    controller_ready: bool,
}

impl ImportOwnership {
    fn begin(&mut self) {
        self.attempted = true;
    }

    fn controller_ready(&mut self) {
        self.controller_ready = true;
    }

    fn rollback_controller_ready(&self) -> Option<bool> {
        self.attempted.then_some(self.controller_ready)
    }
}

impl CancelOnDrop {
    fn new(cancellation: OperationCancellation) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

struct AttachedSession {
    #[allow(
        dead_code,
        reason = "retaining the host session retains source-bound liveness"
    )]
    host: SimulationHostSession,
    definition: String,
    member: WorldMember,
    supervisor_endpoint: String,
}

impl WebotsAttachments {
    #[must_use]
    pub fn new(
        instance: WorldInstanceId,
        world: World,
        project_root: PathBuf,
        native: Arc<HostServer>,
        evidence: Arc<EvidenceSession>,
    ) -> Self {
        Self {
            instance,
            world,
            project_root,
            native,
            evidence,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            workers: Arc::new(Mutex::new(JoinSet::new())),
            cancellations: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    async fn cancel_and_join_workers(&self) -> Result<()> {
        {
            let mut cancellations = self
                .cancellations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cancellations.retain(|cancellation| {
                if let Some(cancellation) = cancellation.upgrade() {
                    cancellation.store(true, Ordering::Release);
                    true
                } else {
                    false
                }
            });
        }
        let mut failures = Vec::new();
        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("attachment worker failed: {}", failures.join("; "))
        }
    }

    /// Reconcile one supervisor-initiated Removing transition without blocking other sessions.
    pub async fn reconcile_removals(&self, runtime: &WorldRuntime) -> Result<()> {
        let candidate = {
            // Attachment owns this mutex through bounded native mutations. The host's health
            // loop must remain free to classify shared-process loss during that transaction.
            let Ok(sessions) = self.sessions.try_lock() else {
                return Ok(());
            };
            let mut candidate = None;
            for (key, session) in sessions.iter() {
                if let Some(failure) = self.native.robot_failure(session.member.execution) {
                    candidate = Some(match failure {
                        NativeRobotFailure::Controller(_) => (
                            key.clone(),
                            WorldMemberEndReason::ControllerFault,
                            Some(SimulationEndReason::ControllerLost),
                            true,
                        ),
                        NativeRobotFailure::SupervisorLost => (
                            key.clone(),
                            WorldMemberEndReason::SupervisorLost,
                            None,
                            false,
                        ),
                    });
                    break;
                }
                match session.host.attachment().await {
                    Ok(Some(attachment))
                        if attachment.phase
                            == phoxal::supervisor::api::simulation::SimulationAttachmentPhase::Removing =>
                    {
                        candidate = Some((
                            key.clone(),
                            WorldMemberEndReason::Stopped,
                            None,
                            true,
                        ));
                        break;
                    }
                    Ok(None) | Err(_) => {
                        candidate = Some((
                            key.clone(),
                            WorldMemberEndReason::SupervisorLost,
                            None,
                            false,
                        ));
                        break;
                    }
                    Ok(Some(_)) => {}
                }
            }
            candidate
        };
        let Some((key, reason, request_end, acknowledge)) = candidate else {
            return Ok(());
        };
        let session = self
            .sessions
            .lock()
            .await
            .remove(&key)
            .context("member cleanup candidate disappeared")?;
        self.finish_removal(runtime, session, reason, request_end, acknowledge)
            .await
    }

    /// End every retained execution before the native world process exits.
    pub async fn end_all(&self, runtime: &WorldRuntime, reason: SimulationEndReason) -> Result<()> {
        self.cancel_and_join_workers().await?;
        let member_reason = match reason {
            SimulationEndReason::WorldStopped => WorldMemberEndReason::Stopped,
            SimulationEndReason::ControllerLost => WorldMemberEndReason::ControllerFault,
            _ => WorldMemberEndReason::AttachmentFailed,
        };
        let mut failures = Vec::new();
        loop {
            let session = {
                let mut sessions = self.sessions.lock().await;
                let Some(key) = sessions.keys().next().cloned() else {
                    break;
                };
                sessions.remove(&key)
            };
            let Some(session) = session else {
                continue;
            };
            if let Err(error) = self
                .finish_removal(runtime, session, member_reason, Some(reason), true)
                .await
            {
                failures.push(format!("{error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("member cleanup failed: {}", failures.join("; "))
        }
    }

    async fn finish_removal(
        &self,
        runtime: &WorldRuntime,
        session: AttachedSession,
        reason: WorldMemberEndReason,
        request_end: Option<SimulationEndReason>,
        acknowledge: bool,
    ) -> Result<()> {
        let _operation = runtime.lock_operation().await;
        let was_running = matches!(
            runtime.snapshot().lifecycle,
            phoxal::world::api::session::WorldLifecycle::Ready {
                motion: phoxal::world::api::session::WorldMotion::Running
            }
        );
        let mut cleanup_failures = Vec::new();
        let mut isolation_failure = runtime
            .pause_native_for_operation()
            .await
            .err()
            .map(|error| format!("native pause failed: {error}"));
        if let Err(error) = runtime.update_state(|state| {
            let Some(member) = state
                .members
                .iter_mut()
                .find(|member| member.execution == session.member.execution)
            else {
                return Ok(false);
            };
            member.phase = WorldMemberPhase::Removing;
            Ok(true)
        }) {
            isolation_failure = Some(format!("failed to publish Removing: {error}"));
        }
        if let Some(failure) = isolation_failure {
            if let Err(error) = runtime.fail(SimulationEndReason::RemovalFailed) {
                cleanup_failures.push(format!("failed to publish fatal removal state: {error}"));
            }
            cleanup_failures.push(failure);
            if let Err(error) = session.host.end(SimulationEndReason::RemovalFailed).await {
                cleanup_failures.push(format!("supervisor failure end failed: {error}"));
            }
            if let Err(error) = session.host.close().await {
                cleanup_failures.push(format!("host session close failed: {error}"));
            }
            let (actuation, dropped_actuation) = self
                .native
                .take_actuation_evidence(session.member.execution);
            let actuation_path = self.evidence.write_actuation(
                session.member.execution,
                actuation,
                dropped_actuation,
            )?;
            self.evidence
                .write_member(&world_member_evidence(WorldMemberTerminal {
                    execution: session.member.execution,
                    robot: session.member.robot,
                    controller: session.member.controller,
                    spawn: session.member.spawn,
                    reason,
                    last_progress: runtime.snapshot().progress,
                    cleanup: WorldMemberCleanup::Incomplete {
                        detail: cleanup_failures.join("; "),
                    },
                    evidence_paths: vec![actuation_path],
                }))?;
            anyhow::bail!(cleanup_failures.join("; "));
        }
        if let Some(end_reason) = request_end
            && let Err(error) = session.host.end(end_reason).await
        {
            cleanup_failures.push(format!("supervisor end failed: {error}"));
        }

        self.native.retire_robot(session.member.execution);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.native.robot_is_parked(session.member.execution)
            && tokio::time::Instant::now() < deadline
            && !matches!(
                self.native.snapshot().lifecycle(),
                NativeWorldLifecycle::Failed(_)
            )
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let parked = self.native.robot_is_parked(session.member.execution);
        if !parked {
            cleanup_failures.push("Robot controller did not confirm parked".to_owned());
            if let Err(error) = runtime.fail(SimulationEndReason::RemovalFailed) {
                cleanup_failures.push(format!("failed to publish failed isolation: {error}"));
            }
        }
        let mut removed = false;
        if parked
            && !matches!(
                self.native.snapshot().lifecycle(),
                NativeWorldLifecycle::Failed(_)
            )
        {
            let removal = tokio::task::spawn_blocking({
                let native = Arc::clone(&self.native);
                let definition = session.definition.clone();
                move || native.remove_robot(definition)
            })
            .await;
            match removal {
                Ok(Ok(())) => removed = true,
                Ok(Err(error)) => {
                    cleanup_failures.push(format!("native removal failed: {error:#}"))
                }
                Err(error) => {
                    cleanup_failures.push(format!("native removal worker failed: {error}"))
                }
            }
        } else if parked {
            cleanup_failures.push("native world failed before Robot removal".to_owned());
        }
        let progress = runtime.snapshot().progress;
        if removed {
            self.native.release_robot(session.member.execution);
            if let Err(error) =
                cleanup_robot_assets(&self.project_root, session.member.execution).await
            {
                cleanup_failures.push(format!("staged asset cleanup failed: {error:#}"));
            }
            if let Err(error) = runtime.update_state(|state| {
                let before = state.members.len();
                state
                    .members
                    .retain(|member| member.execution != session.member.execution);
                Ok(state.members.len() != before)
            }) {
                cleanup_failures.push(format!("failed to publish member removal: {error}"));
            }
        }

        if acknowledge
            && cleanup_failures.is_empty()
            && let Err(error) = session.host.acknowledge_removal().await
        {
            cleanup_failures.push(format!(
                "supervisor removal acknowledgement failed: {error}"
            ));
        }
        if let Err(error) = session.host.close().await {
            cleanup_failures.push(format!("host session close failed: {error}"));
        }
        if !cleanup_failures.is_empty()
            && let Err(error) = runtime.fail(SimulationEndReason::RemovalFailed)
        {
            cleanup_failures.push(format!("failed to publish removal failure: {error}"));
        }
        if was_running
            && cleanup_failures.is_empty()
            && !matches!(
                runtime.snapshot().lifecycle,
                phoxal::world::api::session::WorldLifecycle::Stopping
                    | phoxal::world::api::session::WorldLifecycle::Failed { .. }
            )
            && let Err(error) = runtime.restore_native_after_operation(true).await
        {
            cleanup_failures.push(format!("native resume failed: {error}"));
            if let Err(publish) = runtime.fail(SimulationEndReason::RemovalFailed) {
                cleanup_failures.push(format!(
                    "failed to publish fatal removal resume state: {publish}"
                ));
            }
        }
        let cleanup = if cleanup_failures.is_empty() {
            WorldMemberCleanup::Complete
        } else {
            WorldMemberCleanup::Incomplete {
                detail: cleanup_failures.join("; "),
            }
        };
        let (actuation, dropped_actuation) = self
            .native
            .take_actuation_evidence(session.member.execution);
        let actuation_path = self.evidence.write_actuation(
            session.member.execution,
            actuation,
            dropped_actuation,
        )?;
        self.evidence
            .write_member(&world_member_evidence(WorldMemberTerminal {
                execution: session.member.execution,
                robot: session.member.robot,
                controller: session.member.controller,
                spawn: session.member.spawn,
                reason,
                last_progress: progress,
                cleanup,
                evidence_paths: vec![actuation_path],
            }))?;
        if cleanup_failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(cleanup_failures.join("; "))
        }
    }

    async fn attach_inner(
        &self,
        runtime: &WorldRuntime,
        execution: ExecutionId,
        supervisor_endpoint: String,
        requested_spawn: Option<SpawnId>,
        cancellation: &OperationCancellation,
    ) -> Result<WorldSessionState> {
        let _operation = runtime.lock_operation().await;
        cancellation.check()?;
        let mut sessions = self.sessions.lock().await;
        cancellation.check()?;
        let (spawn, initial_pose) = resolve_spawn(&self.world, requested_spawn)?;
        if let Some(existing) = sessions.get(&execution.to_string()) {
            ensure_idempotent_request(
                execution,
                &existing.member.spawn,
                &existing.supervisor_endpoint,
                &spawn,
                &supervisor_endpoint,
            )?;
            return Ok(runtime.snapshot());
        }
        ensure_attach_slot(&runtime.snapshot().members, execution, &spawn)?;
        let host = SimulationHostSession::connect(SimulationHostConnectOptions::new(
            &supervisor_endpoint,
            format!("webots-world-host-{}", self.instance),
        ))
        .await
        .context("failed to join the fresh execution as its world host")?;
        if let Err(error) = cancellation.check() {
            let _ = host.close().await;
            return Err(error);
        }
        let preparation =
            async {
                ensure!(
                    host.execution() == execution,
                    "session endpoint resolved execution {}, expected {execution}",
                    host.execution()
                );
                let asset_ids = RobotSimulationPlan::required_assets(host.robot())?;
                let mut assets = BTreeMap::new();
                for id in asset_ids {
                    assets.insert(
                        id.clone(),
                        host.assets()
                            .read(&id)
                            .await
                            .with_context(|| format!("failed to preflight asset {id}"))?,
                    );
                    cancellation.check()?;
                }
                let materials = assets.iter().try_fold(
                    std::collections::BTreeSet::new(),
                    |mut dependencies, (id, bytes)| {
                        dependencies.extend(crate::obj::material_dependencies(id, bytes)?);
                        Ok::<_, anyhow::Error>(dependencies)
                    },
                )?;
                for id in materials {
                    if let std::collections::btree_map::Entry::Vacant(entry) = assets.entry(id) {
                        let bytes = host.assets().read(entry.key()).await.with_context(|| {
                            format!("failed to preflight mesh material {}", entry.key())
                        })?;
                        entry.insert(bytes);
                        cancellation.check()?;
                    }
                }
                let mut collision_assets = host
                    .robot()
                    .structure()
                    .links()
                    .flat_map(|link| link.collisions())
                    .filter_map(|collision| collision.geometry().asset_id().cloned())
                    .collect::<std::collections::BTreeSet<_>>();
                for component in host.robot().components() {
                    collision_assets.extend(
                        component
                            .component_type()
                            .structure()
                            .links()
                            .flat_map(|link| link.collisions())
                            .filter_map(|collision| collision.geometry().asset_id().cloned()),
                    );
                }
                for collision in collision_assets {
                    crate::obj::decode(&collision, &assets)?.validate_collision()
                .with_context(|| {
                    format!("Robot collision asset {collision} exceeds the accepted Webots subset")
                })?;
                }
                let step_ms = i32::try_from(self.world.time_step_ns() / 1_000_000)
                    .context("world time step does not fit Webots milliseconds")?;
                let plan = RobotSimulationPlan::derive(host.robot(), step_ms, |id| {
                    assets
                        .get(id)
                        .cloned()
                        .ok_or_else(|| format!("asset {id} was not prefetched"))
                })?;
                stage_robot_assets(&self.project_root, execution, &assets).await?;
                cancellation.check()?;
                let definition = robot_definition(execution);
                let source = render_robot(
                    host.robot(),
                    &plan,
                    &assets,
                    execution,
                    initial_pose,
                    &supervisor_endpoint,
                    self.native.endpoint(),
                )
                .context("failed to render the admitted native Robot")?;
                crate::protocol::validate_robot_import(&definition, &source)
                    .context("generated Robot exceeds the native import budget")?;
                let _: webots_proto::Proto = source
                    .parse()
                    .context("generated native Robot did not parse as R2025a VRML")?;
                cancellation.check()?;
                Ok::<_, anyhow::Error>((plan, definition, source))
            }
            .await;
        let (plan, definition, source) = match preparation {
            Ok(preparation) => preparation,
            Err(error) => {
                let _ = cleanup_robot_assets(&self.project_root, execution).await;
                let _ = host.close().await;
                return Err(error);
            }
        };

        let was_running = matches!(
            runtime.snapshot().lifecycle,
            phoxal::world::api::session::WorldLifecycle::Ready {
                motion: phoxal::world::api::session::WorldMotion::Running
            }
        );
        if let Err(error) = runtime.pause_native_for_operation().await {
            let cleanup = cleanup_robot_assets(&self.project_root, execution).await;
            let close = host.close().await;
            let publish = runtime.fail(SimulationEndReason::MutationFailed);
            return Err(anyhow::anyhow!(error))
                .context("failed to pause before native Robot import")
                .with_context(|| {
                    format!(
                        "fatal world publication: {publish:?}; staged asset cleanup: {cleanup:?}; host close: {close:?}"
                    )
                });
        }
        let mut import = ImportOwnership::default();
        let mut supervisor_transaction = false;
        let mut public_member = false;
        let operation = async {
            cancellation.check()?;
            self.native
                .reserve_robot(execution, plan)
                .map_err(|error| anyhow::anyhow!("failed to reserve Robot plan: {error:?}"))?;

            // Once the mutation request can reach Webots, its outcome is conservative: even an
            // error may follow a partial scene import and therefore owns rollback removal.
            import.begin();
            let native_import = tokio::task::spawn_blocking({
                let native = Arc::clone(&self.native);
                let definition = definition.clone();
                move || native.import_robot(execution, definition, source)
            })
            .await
            .context("native import worker failed")?;
            native_import.context("native Robot import failed")?;
            cancellation.check()?;

            let controller = wait_for_controller(&self.native, execution, cancellation).await?;
            import.controller_ready();
            let boundary = runtime.snapshot().progress;
            let request = AttachRequest::validated(
                self.instance,
                controller,
                boundary,
                self.world.time_step_ns(),
            )?;
            let transaction = host
                .begin_attach(request)
                .await
                .context("failed to begin the supervisor attachment transaction")?;
            supervisor_transaction = true;
            cancellation.check()?;
            let preparing = transaction.initial();
            let member = WorldMember {
                execution,
                robot: host.robot().id().clone(),
                controller,
                phase: match preparing.phase {
                    phoxal::supervisor::api::simulation::SimulationAttachmentPhase::Preparing => {
                        WorldMemberPhase::Preparing
                    }
                    phoxal::supervisor::api::simulation::SimulationAttachmentPhase::Active => {
                        WorldMemberPhase::Preparing
                    }
                    phoxal::supervisor::api::simulation::SimulationAttachmentPhase::Removing => {
                        anyhow::bail!("attachment entered Removing before its Active commit")
                    }
                },
                attached_at: preparing.attached_at,
                spawn: spawn.clone(),
                initial_pose,
            };
            runtime
                .update_state(|state| {
                    if state
                        .members
                        .iter()
                        .any(|existing| existing.execution == execution)
                    {
                        return Err(format!("execution {execution} joined twice"));
                    }
                    if state.members.iter().any(|existing| existing.spawn == spawn) {
                        return Err(format!("spawn point '{spawn}' became occupied"));
                    }
                    state.members.push(member);
                    Ok(true)
                })
                .map_err(anyhow::Error::msg)?;
            public_member = true;
            cancellation.check()?;
            let response = transaction
                .commit()
                .await
                .context("supervisor attachment transaction failed")?;
            cancellation.check()?;
            wait_for_active_ack(
                &self.native,
                execution,
                response.attachment.revision,
                cancellation,
            )
            .await?;
            let member = WorldMember {
                execution,
                robot: host.robot().id().clone(),
                controller,
                phase: WorldMemberPhase::Active,
                attached_at: response.attachment.attached_at,
                spawn: spawn.clone(),
                initial_pose,
            };
            let state = runtime
                .update_state(|state| {
                    if let Some(existing) = state
                        .members
                        .iter_mut()
                        .find(|existing| existing.execution == execution)
                    {
                        *existing = member;
                    } else {
                        if state.members.iter().any(|existing| existing.spawn == spawn) {
                            return Err(format!("spawn point '{spawn}' became occupied"));
                        }
                        state.members.push(member);
                    }
                    Ok(true)
                })
                .map_err(anyhow::Error::msg)?;
            public_member = true;
            cancellation.check()?;
            Ok::<_, anyhow::Error>(state)
        }
        .await;

        let failure = match operation {
            Ok(state) => {
                if let Err(error) = cancellation.check() {
                    error
                } else if let Err(error) = runtime.restore_native_after_operation(was_running).await
                {
                    let publish = runtime.fail(SimulationEndReason::MutationFailed);
                    anyhow::Error::msg(error)
                        .context("failed to restore native motion after Robot attachment")
                        .context(format!("fatal world publication: {publish:?}"))
                } else if let Err(error) = cancellation.check() {
                    error
                } else {
                    let member = state
                        .members
                        .iter()
                        .find(|member| member.execution == execution)
                        .cloned()
                        .context("committed world member disappeared")?;
                    sessions.insert(
                        execution.to_string(),
                        AttachedSession {
                            host,
                            definition,
                            member,
                            supervisor_endpoint,
                        },
                    );
                    return Ok(state);
                }
            }
            Err(error) => error,
        };

        let failed_member = runtime
            .snapshot()
            .members
            .iter()
            .find(|member| member.execution == execution)
            .cloned();
        let mut cleanup_failures = Vec::new();
        let isolated = match runtime.pause_native_for_operation().await {
            Ok(_) => true,
            Err(error) => {
                cleanup_failures.push(format!(
                    "failed to isolate native world for attachment rollback: {error}"
                ));
                if let Err(publish) = runtime.fail(SimulationEndReason::RemovalFailed) {
                    cleanup_failures
                        .push(format!("failed to publish fatal rollback state: {publish}"));
                }
                false
            }
        };
        let mut removing = false;
        if supervisor_transaction {
            match host.end(SimulationEndReason::MutationFailed).await {
                Ok(_) => removing = true,
                Err(error) => {
                    cleanup_failures.push(format!("supervisor rollback end failed: {error}"))
                }
            }
        }
        if public_member
            && let Err(error) = runtime.update_state(|state| {
                state.members.retain(|member| member.execution != execution);
                Ok(true)
            })
        {
            cleanup_failures.push(format!("failed to publish attachment rollback: {error}"));
        }
        if let Some(controller_ready) = import.rollback_controller_ready().filter(|_| isolated) {
            if let Err(error) =
                rollback_import(&self.native, execution, &definition, controller_ready).await
            {
                cleanup_failures.push(format!("native rollback failed: {error:#}"));
            }
        } else if !import.attempted {
            self.native.release_robot(execution);
        } else {
            cleanup_failures.push(
                "native Robot retained because rollback isolation was not confirmed".to_owned(),
            );
        }
        if let Err(error) = cleanup_robot_assets(&self.project_root, execution).await {
            cleanup_failures.push(format!("staged asset rollback failed: {error:#}"));
        }
        if removing
            && cleanup_failures.is_empty()
            && let Err(error) = host.acknowledge_removal().await
        {
            cleanup_failures.push(format!(
                "supervisor rollback acknowledgement failed: {error}"
            ));
        }
        if let Err(error) = host.close().await {
            cleanup_failures.push(format!("host rollback close failed: {error}"));
        }
        if !cleanup_failures.is_empty()
            && !matches!(
                runtime.snapshot().lifecycle,
                phoxal::world::api::session::WorldLifecycle::Failed { .. }
            )
            && let Err(error) = runtime.fail(SimulationEndReason::MutationFailed)
        {
            cleanup_failures.push(format!("failed to publish fatal rollback state: {error}"));
        }
        if was_running
            && isolated
            && cleanup_failures.is_empty()
            && !matches!(
                runtime.snapshot().lifecycle,
                phoxal::world::api::session::WorldLifecycle::Failed { .. }
                    | phoxal::world::api::session::WorldLifecycle::Stopping
            )
            && let Err(error) = runtime.restore_native_after_operation(true).await
        {
            cleanup_failures.push(format!("native rollback resume failed: {error}"));
            if let Err(publish) = runtime.fail(SimulationEndReason::MutationFailed) {
                cleanup_failures.push(format!(
                    "failed to publish fatal rollback resume state: {publish}"
                ));
            }
        }
        if let Some(member) = failed_member {
            let cleanup = if cleanup_failures.is_empty() {
                WorldMemberCleanup::Complete
            } else {
                WorldMemberCleanup::Incomplete {
                    detail: cleanup_failures.join("; "),
                }
            };
            let (actuation, dropped_actuation) =
                self.native.take_actuation_evidence(member.execution);
            let actuation_path =
                self.evidence
                    .write_actuation(member.execution, actuation, dropped_actuation)?;
            self.evidence
                .write_member(&world_member_evidence(WorldMemberTerminal {
                    execution: member.execution,
                    robot: member.robot,
                    controller: member.controller,
                    spawn: member.spawn,
                    reason: WorldMemberEndReason::AttachmentFailed,
                    last_progress: runtime.snapshot().progress,
                    cleanup,
                    evidence_paths: vec![actuation_path],
                }))?;
        }
        if cleanup_failures.is_empty() {
            Err(failure)
        } else {
            Err(failure.context(format!(
                "attachment rollback was incomplete: {}",
                cleanup_failures.join("; ")
            )))
        }
    }
}

fn ensure_idempotent_request(
    execution: ExecutionId,
    existing_spawn: &SpawnId,
    existing_endpoint: &str,
    requested_spawn: &SpawnId,
    requested_endpoint: &str,
) -> Result<()> {
    ensure!(
        existing_spawn == requested_spawn,
        "idempotent execution {execution} retry changed its resolved spawn"
    );
    ensure!(
        existing_endpoint == requested_endpoint,
        "idempotent execution {execution} retry changed its supervisor endpoint"
    );
    Ok(())
}

fn ensure_attach_slot(
    members: &[WorldMember],
    execution: ExecutionId,
    spawn: &SpawnId,
) -> Result<()> {
    ensure!(
        !members.iter().any(|member| member.execution == execution),
        "world state already contains execution {execution} without a retained host session"
    );
    ensure!(
        !members.iter().any(|member| &member.spawn == spawn),
        "spawn point '{spawn}' is already occupied"
    );
    Ok(())
}

impl AttachmentOperation for WebotsAttachments {
    fn attach<'a>(
        &'a self,
        runtime: &'a WorldRuntime,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> HostOperation<'a, WorldSessionState> {
        Box::pin(async move {
            let cancellation = OperationCancellation::new();
            let (result_tx, result_rx) = oneshot::channel();
            {
                let mut cancellations = self
                    .cancellations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                cancellations.retain(|existing| existing.strong_count() > 0);
                cancellations.push(Arc::downgrade(&cancellation.0));
            }
            let owned = self.clone();
            let runtime = runtime.clone();
            let worker_cancellation = cancellation.clone();
            let mut workers = self.workers.lock().await;
            while workers.try_join_next().is_some() {}
            workers.spawn(async move {
                let result = owned
                    .attach_inner(
                        &runtime,
                        execution,
                        supervisor_endpoint,
                        spawn,
                        &worker_cancellation,
                    )
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = result_tx.send(result);
            });
            drop(workers);
            let mut cancel_on_drop = CancelOnDrop::new(cancellation);
            let result = result_rx
                .await
                .map_err(|_| "owned world attachment worker exited without a result".to_owned())?;
            cancel_on_drop.disarm();
            result
        })
    }
}

fn resolve_spawn(
    world: &World,
    requested: Option<SpawnId>,
) -> Result<(SpawnId, phoxal::model::structure::Pose)> {
    let spawns = world.spawn_points().collect::<Vec<_>>();
    match requested {
        Some(requested) => spawns
            .into_iter()
            .find(|(id, _)| **id == requested)
            .map(|(id, pose)| (id.clone(), pose))
            .with_context(|| format!("world has no spawn point '{requested}'")),
        None => {
            let [(id, pose)] = spawns.as_slice() else {
                anyhow::bail!(
                    "spawn may be omitted only when the world has exactly one authored spawn point"
                );
            };
            Ok(((*id).clone(), *pose))
        }
    }
}

async fn stage_robot_assets(
    project_root: &Path,
    execution: ExecutionId,
    assets: &BTreeMap<AssetId, Vec<u8>>,
) -> Result<()> {
    let root = project_root
        .join("assets")
        .join("robots")
        .join(execution.to_string());
    let texture_root = project_root
        .join(".phoxal")
        .join("textures")
        .join("robots")
        .join(execution.to_string());
    let result = async {
        tokio::fs::create_dir_all(&root)
            .await
            .with_context(|| format!("failed to create {}", root.display()))?;
        for (id, bytes) in assets {
            let target = root.join(id.as_str());
            ensure!(
                target.starts_with(&root),
                "asset {id} escapes Robot staging"
            );
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&target, bytes)
                .await
                .with_context(|| format!("failed to stage Robot asset {}", target.display()))?;
            if Path::new(id.as_str())
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                == Some("glb")
            {
                let decoded = DecodedMesh::decode(bytes)
                    .with_context(|| format!("failed to decode staged Robot GLB {id}"))?;
                stage_decoded_images(&texture_root, id.as_str(), &decoded)?;
            }
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&root).await;
        let _ = tokio::fs::remove_dir_all(&texture_root).await;
    }
    result
}

async fn wait_for_controller(
    native: &HostServer,
    execution: ExecutionId,
    cancellation: &OperationCancellation,
) -> Result<phoxal::identity::ProducerId> {
    let deadline = tokio::time::Instant::now() + CONTROLLER_READY_TIMEOUT;
    loop {
        cancellation.check()?;
        if let Some(controller) = native.robot_controller(execution) {
            return Ok(controller);
        }
        if let NativeWorldLifecycle::Failed(failure) = native.snapshot().lifecycle() {
            anyhow::bail!("native world failed while Robot started: {failure:?}");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "Robot controller did not become ready within {CONTROLLER_READY_TIMEOUT:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_for_active_ack(
    native: &HostServer,
    execution: ExecutionId,
    revision: u64,
    cancellation: &OperationCancellation,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + CONTROLLER_READY_TIMEOUT;
    loop {
        cancellation.check()?;
        if native.robot_active_revision(execution) == Some(revision) {
            return Ok(());
        }
        if let NativeWorldLifecycle::Failed(failure) = native.snapshot().lifecycle() {
            anyhow::bail!("native world failed before Robot acknowledged Active: {failure:?}");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "Robot controller did not acknowledge Active revision {revision} within {CONTROLLER_READY_TIMEOUT:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn cleanup_robot_assets(project_root: &Path, execution: ExecutionId) -> Result<()> {
    let roots = [
        project_root
            .join("assets")
            .join("robots")
            .join(execution.to_string()),
        project_root
            .join(".phoxal")
            .join("textures")
            .join("robots")
            .join(execution.to_string()),
    ];
    for root in roots {
        match tokio::fs::remove_dir_all(&root).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to remove staged Robot assets for {execution}")
                });
            }
        }
    }
    Ok(())
}

async fn rollback_import(
    native: &Arc<HostServer>,
    execution: ExecutionId,
    definition: &str,
    controller_ready: bool,
) -> Result<()> {
    if controller_ready {
        native.retire_robot(execution);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !native.robot_is_parked(execution)
            && tokio::time::Instant::now() < deadline
            && !matches!(
                native.snapshot().lifecycle(),
                NativeWorldLifecycle::Failed(_)
            )
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        ensure!(
            native.robot_is_parked(execution),
            "Robot controller did not confirm parked during rollback"
        );
    }
    let result = tokio::task::spawn_blocking({
        let native = Arc::clone(native);
        let definition = definition.to_owned();
        move || native.rollback_robot(definition)
    })
    .await
    .context("native rollback worker failed")?;
    result.context("native Robot rollback failed")?;
    native.release_robot(execution);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::bus::RobotInstant;
    use phoxal::identity::{ProducerId, TimelineId};
    use phoxal::model::identity::RobotId;
    use phoxal::model::world::{LiveAttachmentBoundary, WorldProgress};

    fn pose(x: f64) -> phoxal::model::structure::Pose {
        serde_json::from_value(serde_json::json!({
            "xyz": [x, 0.0, 0.0],
            "rpy": [0.0, 0.0, 0.0]
        }))
        .expect("pose")
    }

    fn world(spawns: &[(&str, f64)]) -> World {
        let spawn_points = spawns
            .iter()
            .map(|(id, x)| {
                (
                    (*id).to_owned(),
                    serde_json::to_value(pose(*x)).expect("pose JSON"),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::from_value(serde_json::json!({
            "id": "test-world",
            "time_step_ns": 12_000_000,
            "gravity_mps2": [0.0, 0.0, -9.81],
            "spawn_points": spawn_points,
            "entities": []
        }))
        .expect("world")
    }

    fn member(execution: ExecutionId, spawn: SpawnId) -> WorldMember {
        WorldMember {
            execution,
            robot: RobotId::new("robot").expect("robot id"),
            controller: ProducerId::try_from(0x3000_0000_0000_0000_0000_0000_0000_0003)
                .expect("producer"),
            phase: WorldMemberPhase::Active,
            attached_at: LiveAttachmentBoundary {
                world: WorldProgress::zero(12_000_000).expect("progress"),
                execution: RobotInstant::new(TimelineId::from_raw(1).expect("timeline"), 0),
            },
            spawn,
            initial_pose: pose(0.0),
        }
    }

    #[test]
    fn omitted_spawn_requires_exactly_one_authored_point() {
        let one = world(&[("only", 2.0)]);
        let (spawn, resolved) = resolve_spawn(&one, None).expect("sole spawn resolves");
        assert_eq!(spawn.as_str(), "only");
        assert_eq!(resolved.xyz(), [2.0, 0.0, 0.0]);
        assert!(resolve_spawn(&world(&[]), None).is_err());
        assert!(resolve_spawn(&world(&[("first", 0.0), ("second", 1.0)]), None).is_err());
    }

    #[test]
    fn duplicate_spawn_and_conflicting_idempotent_retries_fail_before_mutation() {
        let first =
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001).expect("execution");
        let second =
            ExecutionId::try_from(0x2000_0000_0000_0000_0000_0000_0000_0002).expect("execution");
        let spawn = SpawnId::new("west-bay").expect("spawn");
        let other = SpawnId::new("east-bay").expect("spawn");
        let members = vec![member(first, spawn.clone())];
        assert!(ensure_attach_slot(&members, second, &spawn).is_err());
        ensure_attach_slot(&members, second, &other)
            .expect("a second member may reserve the distinct authored spawn");
        assert!(
            ensure_attach_slot(&members, first, &SpawnId::new("other").expect("spawn")).is_err()
        );

        ensure_idempotent_request(first, &spawn, "tcp://one", &spawn, "tcp://one")
            .expect("exact retry");
        assert!(
            ensure_idempotent_request(
                first,
                &spawn,
                "tcp://one",
                &SpawnId::new("other").expect("spawn"),
                "tcp://one"
            )
            .is_err()
        );
        assert!(
            ensure_idempotent_request(first, &spawn, "tcp://one", &spawn, "tcp://two").is_err()
        );
    }

    #[tokio::test]
    async fn dropping_a_request_at_an_await_cancels_its_owned_worker_cleanup() {
        let cancellation = OperationCancellation::new();
        let worker_cancellation = cancellation.clone();
        let cleaned = Arc::new(AtomicBool::new(false));
        let worker_cleaned = Arc::clone(&cleaned);
        let worker = tokio::spawn(async move {
            loop {
                if worker_cancellation.check().is_err() {
                    worker_cleaned.store(true, Ordering::Release);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        let (started_tx, started_rx) = oneshot::channel();
        let request = tokio::spawn(async move {
            let _cancel_on_drop = CancelOnDrop::new(cancellation);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("request reached its await point");
        request.abort();
        worker.await.expect("owned cleanup worker converged");
        assert!(cleaned.load(Ordering::Acquire));
    }

    #[test]
    fn failed_import_attempt_still_owns_idempotent_native_removal() {
        let mut ownership = ImportOwnership::default();
        ownership.begin();
        let native_result = Result::<(), &'static str>::Err("partial native import");
        assert!(native_result.is_err());
        assert_eq!(ownership.rollback_controller_ready(), Some(false));
    }
}
