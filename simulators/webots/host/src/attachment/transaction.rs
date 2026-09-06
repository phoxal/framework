use super::*;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum AttachmentTransactionPhase {
    Prepared,
    NativeImportAttempted,
    NativeControllerReady,
    SupervisorPreparing,
    MemberPreparing,
    Active,
}

impl AttachmentTransactionPhase {
    fn import_attempted(self) -> bool {
        self >= Self::NativeImportAttempted
    }

    pub(super) fn controller_ready(self) -> Option<bool> {
        self.import_attempted()
            .then_some(self >= Self::NativeControllerReady)
    }

    fn supervisor_started(self) -> bool {
        self >= Self::SupervisorPreparing
    }

    fn member_published(self) -> bool {
        self >= Self::MemberPreparing
    }
}

impl WebotsAttachments {
    pub(super) async fn attach_inner(
        &self,
        runtime: &WorldRuntime,
        execution: ExecutionId,
        supervisor_endpoint: String,
        requested_spawn: Option<SpawnId>,
        cancellation: &OperationCancellation,
    ) -> Result<WorldSessionState> {
        let _operation = runtime.lock_operation().await;
        cancellation.check()?;
        ensure!(
            matches!(
                runtime.snapshot().lifecycle,
                phoxal::world::api::session::WorldLifecycle::Ready { .. }
            ),
            "world attachment requires a Ready world"
        );
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
        let PreparedRobot {
            host,
            assets: staged_assets,
            plan,
            definition,
            source,
        } = prepare_robot(
            self,
            host,
            execution,
            &supervisor_endpoint,
            initial_pose,
            cancellation,
        )
        .await?;

        let was_running = matches!(
            runtime.snapshot().lifecycle,
            phoxal::world::api::session::WorldLifecycle::Ready {
                motion: phoxal::world::api::session::WorldMotion::Running
            }
        );
        if let Err(error) = runtime.pause_native_for_operation().await {
            let cleanup = staged_assets.cleanup().await;
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
        let mut phase = AttachmentTransactionPhase::Prepared;
        let operation = async {
            cancellation.check()?;
            self.native
                .reserve_robot(execution, plan)
                .map_err(|error| anyhow::anyhow!("failed to reserve Robot plan: {error:?}"))?;
            // Once the mutation request can reach Webots, its outcome is conservative: even an
            // error may follow a partial scene import and therefore owns rollback removal.
            phase = AttachmentTransactionPhase::NativeImportAttempted;
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
            phase = AttachmentTransactionPhase::NativeControllerReady;
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
            phase = AttachmentTransactionPhase::SupervisorPreparing;
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
            runtime.prepare_member(member).map_err(anyhow::Error::msg)?;
            phase = AttachmentTransactionPhase::MemberPreparing;
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
                .activate_member(member)
                .map_err(anyhow::Error::msg)?;
            phase = AttachmentTransactionPhase::Active;
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
                            assets: staged_assets,
                        },
                    );
                    return Ok(runtime.snapshot());
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
        if phase.supervisor_started() {
            match host.end(SimulationEndReason::MutationFailed).await {
                Ok(_) => removing = true,
                Err(error) => {
                    cleanup_failures.push(format!("supervisor rollback end failed: {error}"))
                }
            }
        }
        if phase.member_published()
            && let Err(error) = runtime.complete_member_removal(execution)
        {
            cleanup_failures.push(format!("failed to publish attachment rollback: {error}"));
        }
        if let Some(controller_ready) = phase.controller_ready().filter(|_| isolated) {
            if let Err(error) =
                rollback_import(&self.native, execution, &definition, controller_ready).await
            {
                cleanup_failures.push(format!("native rollback failed: {error:#}"));
            }
        } else if !phase.import_attempted() {
            self.native.release_robot(execution);
        } else {
            cleanup_failures.push(
                "native Robot retained because rollback isolation was not confirmed".to_owned(),
            );
        }
        if let Err(error) = staged_assets.cleanup().await {
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

impl WebotsAttachments {
    pub fn attach<'a>(
        &'a self,
        runtime: &'a WorldRuntime,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> phoxal::world::WorldSessionOperation<'a, WorldSessionState> {
        Box::pin(async move {
            let (result_tx, result_rx) = oneshot::channel();
            let owned = self.clone();
            let runtime = runtime.clone();
            let cancellation = {
                let mut workers = self.workers.lock().await;
                if let Err(error) = workers.reap_finished() {
                    return Err(format!("{error:#}"));
                }
                if workers.shutdown.is_cancelled() {
                    return Err("world attachment admission is closed".to_owned());
                }
                let cancellation = OperationCancellation::child(&workers.shutdown);
                let worker_cancellation = cancellation.clone();
                workers.tasks.spawn(async move {
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
                cancellation
            };
            let mut cancel_on_drop = CancelOnDrop::new(cancellation);
            let result = result_rx
                .await
                .map_err(|_| "owned world attachment worker exited without a result".to_owned())?;
            cancel_on_drop.disarm();
            result
        })
    }
}

pub(super) async fn rollback_import(
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
