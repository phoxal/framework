use super::*;

/// A gap-free current state plus every strictly newer complete replacement.
pub struct WorldStateSubscription {
    bootstrap: WorldSessionBootstrap,
    current: WorldSessionState,
    updates: WireSubscription<WorldSessionStateStream>,
    last_stream_revision: u64,
    last_stream_progress: crate::model::world::WorldProgress,
}

impl WorldStateSubscription {
    pub(super) fn reconcile(
        bootstrap: WorldSessionBootstrap,
        mut current: WorldSessionState,
        mut updates: WireSubscription<WorldSessionStateStream>,
    ) -> Result<Self, WorldSessionWireError> {
        let mut last_stream_revision = None;
        let mut last_stream_progress = None;
        while let Some(update) = updates.try_recv()? {
            validate_state_against(&bootstrap, &update.state)?;
            validate_stream_revision("state", &mut last_stream_revision, update.state.revision)?;
            validate_stream_progress(&mut last_stream_progress, update.state.progress)?;
            if update.state.revision > current.revision {
                validate_progress_not_before(current.progress, update.state.progress)?;
                current = update.state;
            }
        }
        let last_stream_revision = last_stream_revision.ok_or_else(|| {
            WorldSessionWireError::Protocol(
                "world state subscription did not begin with a snapshot".to_owned(),
            )
        })?;
        let last_stream_progress = last_stream_progress.ok_or_else(|| {
            WorldSessionWireError::Protocol(
                "world state subscription did not begin with progress".to_owned(),
            )
        })?;
        Ok(Self {
            bootstrap,
            current,
            updates,
            last_stream_revision,
            last_stream_progress,
        })
    }

    #[must_use]
    pub fn current(&self) -> &WorldSessionState {
        &self.current
    }

    pub fn try_recv(&mut self) -> Result<Option<&WorldSessionState>, WorldSessionWireError> {
        let Some(update) = self.updates.try_recv()? else {
            return Ok(None);
        };
        validate_state_against(&self.bootstrap, &update.state)?;
        validate_stream_revision(
            "state",
            &mut Some(self.last_stream_revision),
            update.state.revision,
        )?;
        validate_progress_not_before(self.last_stream_progress, update.state.progress)?;
        self.last_stream_revision = update.state.revision;
        self.last_stream_progress = update.state.progress;
        if update.state.revision <= self.current.revision {
            return Ok(None);
        }
        validate_progress_not_before(self.current.progress, update.state.progress)?;
        self.current = update.state;
        Ok(Some(&self.current))
    }

    pub async fn recv(&mut self) -> Result<&WorldSessionState, WorldSessionWireError> {
        loop {
            let update = self.updates.recv().await?;
            validate_state_against(&self.bootstrap, &update.state)?;
            validate_stream_revision(
                "state",
                &mut Some(self.last_stream_revision),
                update.state.revision,
            )?;
            validate_progress_not_before(self.last_stream_progress, update.state.progress)?;
            self.last_stream_revision = update.state.revision;
            self.last_stream_progress = update.state.progress;
            if update.state.revision > self.current.revision {
                validate_progress_not_before(self.current.progress, update.state.progress)?;
                self.current = update.state;
                return Ok(&self.current);
            }
        }
    }

    pub async fn wait_for_member_active(
        &mut self,
        execution: ExecutionId,
    ) -> Result<&WorldSessionState, WorldSessionWireError> {
        loop {
            if self.current.members.iter().any(|member| {
                member.execution == execution && member.phase == WorldMemberPhase::Active
            }) {
                return Ok(&self.current);
            }
            self.recv().await?;
        }
    }
}

/// A gap-free current diagnostics value plus strictly newer replacements.
pub struct WorldDiagnosticsSubscription {
    current: WorldSessionDiagnostics,
    updates: WireSubscription<WorldSessionDiagnosticsStream>,
    last_stream_revision: u64,
}

impl WorldDiagnosticsSubscription {
    pub(super) fn reconcile(
        mut current: WorldSessionDiagnostics,
        mut updates: WireSubscription<WorldSessionDiagnosticsStream>,
    ) -> Result<Self, WorldSessionWireError> {
        current.validate()?;
        let mut last_stream_revision = None;
        while let Some(update) = updates.try_recv()? {
            update.diagnostics.validate()?;
            validate_stream_revision(
                "diagnostics",
                &mut last_stream_revision,
                update.diagnostics.revision,
            )?;
            if update.diagnostics.revision > current.revision {
                current = update.diagnostics;
            }
        }
        let last_stream_revision = last_stream_revision.ok_or_else(|| {
            WorldSessionWireError::Protocol(
                "world diagnostics subscription did not begin with a snapshot".to_owned(),
            )
        })?;
        Ok(Self {
            current,
            updates,
            last_stream_revision,
        })
    }

    #[must_use]
    pub const fn current(&self) -> WorldSessionDiagnostics {
        self.current
    }

    pub fn try_recv(&mut self) -> Result<Option<WorldSessionDiagnostics>, WorldSessionWireError> {
        let Some(update) = self.updates.try_recv()? else {
            return Ok(None);
        };
        update.diagnostics.validate()?;
        validate_stream_revision(
            "diagnostics",
            &mut Some(self.last_stream_revision),
            update.diagnostics.revision,
        )?;
        self.last_stream_revision = update.diagnostics.revision;
        if update.diagnostics.revision <= self.current.revision {
            return Ok(None);
        }
        self.current = update.diagnostics;
        Ok(Some(self.current))
    }

    pub async fn recv(&mut self) -> Result<WorldSessionDiagnostics, WorldSessionWireError> {
        loop {
            let update = self.updates.recv().await?;
            update.diagnostics.validate()?;
            validate_stream_revision(
                "diagnostics",
                &mut Some(self.last_stream_revision),
                update.diagnostics.revision,
            )?;
            self.last_stream_revision = update.diagnostics.revision;
            if update.diagnostics.revision > self.current.revision {
                self.current = update.diagnostics;
                return Ok(self.current);
            }
        }
    }
}

pub(super) struct WireSubscription<T> {
    pub(super) receiver: mpsc::Receiver<Result<T, WorldSessionWireError>>,
    pub(super) task: JoinHandle<()>,
}

impl<T> WireSubscription<T> {
    fn try_recv(&mut self) -> Result<Option<T>, WorldSessionWireError> {
        match self.receiver.try_recv() {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(error)) => Err(error),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(WorldSessionWireError::Closed),
        }
    }

    async fn recv(&mut self) -> Result<T, WorldSessionWireError> {
        self.receiver
            .recv()
            .await
            .ok_or(WorldSessionWireError::Closed)?
    }
}

impl<T> Drop for WireSubscription<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn validate_stream_revision(
    stream: &'static str,
    previous: &mut Option<u64>,
    revision: u64,
) -> Result<(), WorldSessionWireError> {
    if let Some(previous) = *previous
        && revision <= previous
    {
        return Err(WorldSessionWireError::Protocol(format!(
            "world {stream} revision {revision} did not increase beyond {previous}"
        )));
    }
    *previous = Some(revision);
    Ok(())
}

pub(super) fn validate_state_against(
    bootstrap: &WorldSessionBootstrap,
    state: &WorldSessionState,
) -> Result<(), WorldSessionWireError> {
    state.validate()?;
    if state.instance != bootstrap.instance {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "instance" });
    }
    if state.provenance.world != bootstrap.world {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "world" });
    }
    if state.provenance.digest != bootstrap.digest {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "digest" });
    }
    if state.provenance.framework != bootstrap.framework {
        return Err(WorldSessionWireError::BootstrapMismatch { field: "framework" });
    }
    Ok(())
}

fn validate_stream_progress(
    previous: &mut Option<crate::model::world::WorldProgress>,
    progress: crate::model::world::WorldProgress,
) -> Result<(), WorldSessionWireError> {
    if let Some(previous) = *previous {
        validate_progress_not_before(previous, progress)?;
    }
    *previous = Some(progress);
    Ok(())
}

fn validate_progress_not_before(
    previous: crate::model::world::WorldProgress,
    observed: crate::model::world::WorldProgress,
) -> Result<(), WorldSessionWireError> {
    if observed.completed_step() < previous.completed_step()
        || observed.elapsed_ns() < previous.elapsed_ns()
    {
        return Err(WorldSessionWireError::Protocol(format!(
            "world progress regressed from step {} at {} ns to step {} at {} ns",
            previous.completed_step(),
            previous.elapsed_ns(),
            observed.completed_step(),
            observed.elapsed_ns(),
        )));
    }
    Ok(())
}
