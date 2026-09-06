use super::*;

impl WorldRuntime {
    pub(crate) async fn apply_control(
        &self,
        request: WorldControl,
    ) -> Result<WorldSessionState, String> {
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
}
