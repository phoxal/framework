use super::*;

impl WebotsAttachments {
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
        let mut failures = self
            .cancel_and_join_workers()
            .await
            .err()
            .map(|error| vec![format!("attachment worker cleanup failed: {error:#}")])
            .unwrap_or_default();
        let member_reason = match reason {
            SimulationEndReason::WorldStopped => WorldMemberEndReason::Stopped,
            SimulationEndReason::ControllerLost => WorldMemberEndReason::ControllerFault,
            _ => WorldMemberEndReason::AttachmentFailed,
        };
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
        if let Err(error) = runtime.mark_member_removing(session.member.execution) {
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
            if let Err(error) = session.assets.cleanup().await {
                cleanup_failures.push(format!("staged asset cleanup failed: {error:#}"));
            }
            if let Err(error) = runtime.complete_member_removal(session.member.execution) {
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
}
