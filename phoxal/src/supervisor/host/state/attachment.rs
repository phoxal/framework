//! Serialized Live attachment transitions under the execution publication lock.

use super::*;

impl ExecutionState {
    /// The current source-bound Live attachment, if any.
    pub(crate) fn attachment(&self) -> Option<SimulationAttachmentState> {
        self.lock().attachment
    }

    /// Observe current attachment phase for internal liveness enforcement.
    pub(crate) fn subscribe_attachment(
        &self,
    ) -> watch::Receiver<Option<SimulationAttachmentState>> {
        self.inner.attachment_current.subscribe()
    }

    /// Take the one ordered stream of complete attachment replacements.
    pub(crate) fn take_attachment_updates(
        &self,
    ) -> Result<mpsc::Receiver<Option<SimulationAttachmentState>>, AttachmentStateError> {
        self.inner
            .attachment_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(AttachmentStateError::StreamAlreadyTaken)
    }

    /// Bind a proposed world and controller in Preparing without changing the
    /// execution time domain.
    pub(crate) fn prepare_attachment(
        &self,
        host: ProducerId,
        request: AttachRequest,
    ) -> Result<(SimulationAttachmentState, TimeDomain), AttachmentStateError> {
        let mut data = self.lock();
        if data.stopping {
            return Err(AttachmentStateError::Stopping);
        }
        if let Some(current) = data.attachment {
            if current.host == host
                && current.world == request.world()
                && current.controller == request.controller()
                && current.attached_at.world == request.progress()
                && current.phase != SimulationAttachmentPhase::Removing
            {
                if current.phase == SimulationAttachmentPhase::Active
                    && !data.presence.admits_live_controller(current.controller)
                {
                    return Err(AttachmentStateError::ControllerNotReady {
                        controller: current.controller,
                    });
                }
                return Ok((current, data.time_domain));
            }
            return Err(AttachmentStateError::AlreadyAttached {
                world: current.world,
                host: current.host,
                controller: current.controller,
            });
        }
        if data.time_domain.mode != TimeMode::Monotonic {
            return Err(AttachmentStateError::NonMonotonic);
        }
        if !data.presence.admits_live_controller(request.controller()) {
            return Err(AttachmentStateError::ControllerNotReady {
                controller: request.controller(),
            });
        }
        let Some(now) = LocalInstant::try_now() else {
            return Err(AttachmentStateError::ClockUnavailable);
        };
        let permit = reserve_attachment(&self.inner.attachment_updates)?;
        let revision = data
            .attachment_revision
            .checked_add(1)
            .ok_or(AttachmentStateError::RevisionExhausted)?;
        let attachment = SimulationAttachmentState {
            revision,
            world: request.world(),
            host,
            controller: request.controller(),
            phase: SimulationAttachmentPhase::Preparing,
            attached_at: crate::model::world::LiveAttachmentBoundary {
                world: request.progress(),
                execution: RobotInstant::new(data.time_domain.timeline, now.boot_ns()),
            },
        };
        data.attachment_revision = revision;
        data.attachment = Some(attachment);
        self.inner.attachment_current.send_replace(Some(attachment));
        permit.send(Some(attachment));
        Ok((attachment, data.time_domain))
    }

    /// Commit one Preparing transaction after its bound controller has
    /// acknowledged the revision.
    pub(crate) fn activate_attachment(
        &self,
        host: ProducerId,
        preparing_revision: u64,
    ) -> Result<(SimulationAttachmentState, TimeDomain), AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.host != host {
            return Err(AttachmentStateError::WrongHost {
                expected: current.host,
                observed: host,
            });
        }
        if current.phase == SimulationAttachmentPhase::Active {
            return Ok((current, data.time_domain));
        }
        if current.phase != SimulationAttachmentPhase::Preparing
            || current.revision != preparing_revision
        {
            return Err(AttachmentStateError::NotPreparing);
        }
        if !data.presence.admits_live_controller(current.controller) {
            return Err(AttachmentStateError::ControllerNotReady {
                controller: current.controller,
            });
        }
        let permit = reserve_attachment(&self.inner.attachment_updates)?;
        let revision = data
            .attachment_revision
            .checked_add(1)
            .ok_or(AttachmentStateError::RevisionExhausted)?;
        let active = SimulationAttachmentState {
            revision,
            phase: SimulationAttachmentPhase::Active,
            ..current
        };
        data.attachment_revision = revision;
        data.attachment = Some(active);
        self.inner.attachment_current.send_replace(Some(active));
        permit.send(Some(active));
        Ok((active, data.time_domain))
    }

    /// Enter Removing from the bound host. The execution retains the terminal
    /// state until its ordinary supervisor shutdown completes.
    pub(crate) fn remove_attachment(
        &self,
        host: ProducerId,
    ) -> Result<SimulationAttachmentState, AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.host != host {
            return Err(AttachmentStateError::WrongHost {
                expected: current.host,
                observed: host,
            });
        }
        if current.phase == SimulationAttachmentPhase::Removing {
            return Ok(current);
        }
        transition_to_removing(&mut data, &self.inner, current)
    }

    /// Abort exactly one still-Preparing transaction. A delayed waiter cannot
    /// use this to remove a later revision.
    pub(crate) fn abort_preparing_attachment(
        &self,
        host: ProducerId,
        preparing_revision: u64,
    ) -> Result<SimulationAttachmentState, AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.host != host {
            return Err(AttachmentStateError::WrongHost {
                expected: current.host,
                observed: host,
            });
        }
        if current.phase != SimulationAttachmentPhase::Preparing
            || current.revision != preparing_revision
        {
            return Err(AttachmentStateError::NotPreparing);
        }
        transition_to_removing(&mut data, &self.inner, current)
    }

    /// Converge an exact Active revision to Removing after a typed liveness
    /// failure. This is supervisor-owned rather than host-attributed.
    pub(crate) fn fail_active_attachment(
        &self,
        active_revision: u64,
        reason: SimulationEndReason,
    ) -> Result<SimulationAttachmentState, AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.phase != SimulationAttachmentPhase::Active
            || current.revision != active_revision
        {
            return Err(AttachmentStateError::NotActiveRevision);
        }
        data.stopping = true;
        data.attachment_failure = Some(reason);
        transition_to_removing(&mut data, &self.inner, current)
    }

    pub(crate) fn attachment_failure(&self) -> Option<SimulationEndReason> {
        self.lock().attachment_failure
    }

    /// Refuse new attachment work and publish Removing before an intentional
    /// supervisor shutdown tears down the transport.
    pub(crate) fn begin_shutdown_attachment(
        &self,
    ) -> Result<Option<SimulationAttachmentState>, AttachmentStateError> {
        let mut data = self.lock();
        data.stopping = true;
        let Some(current) = data.attachment else {
            return Ok(None);
        };
        if current.phase == SimulationAttachmentPhase::Removing {
            return Ok(Some(current));
        }
        transition_to_removing(&mut data, &self.inner, current).map(Some)
    }

}
fn reserve_attachment(
    sender: &mpsc::Sender<Option<SimulationAttachmentState>>,
) -> Result<mpsc::Permit<'_, Option<SimulationAttachmentState>>, AttachmentStateError> {
    sender.try_reserve().map_err(|error| match error {
        mpsc::error::TrySendError::Full(()) => AttachmentStateError::StreamFull,
        mpsc::error::TrySendError::Closed(()) => AttachmentStateError::StreamClosed,
    })
}
fn transition_to_removing(
    data: &mut Data,
    inner: &Inner,
    current: SimulationAttachmentState,
) -> Result<SimulationAttachmentState, AttachmentStateError> {
    let permit = reserve_attachment(&inner.attachment_updates)?;
    let revision = data
        .attachment_revision
        .checked_add(1)
        .ok_or(AttachmentStateError::RevisionExhausted)?;
    let removing = SimulationAttachmentState {
        revision,
        phase: SimulationAttachmentPhase::Removing,
        ..current
    };
    data.attachment_revision = revision;
    data.attachment = Some(removing);
    inner.attachment_current.send_replace(Some(removing));
    permit.send(Some(removing));
    Ok(removing)
}

/// An attachment transition could not be admitted without losing serialized
/// state or violating source ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AttachmentStateError {
    #[error("the execution attachment stream is already being served")]
    StreamAlreadyTaken,
    #[error("the execution attachment stream is saturated")]
    StreamFull,
    #[error("the execution attachment stream is unavailable")]
    StreamClosed,
    #[error("the execution attachment revision is exhausted")]
    RevisionExhausted,
    #[error("Live attachment requires the unchanged monotonic execution time domain")]
    NonMonotonic,
    #[error(
        "controller {controller} does not exclusively hold every delegated driver Ready lease while all non-drivers are Ready"
    )]
    ControllerNotReady { controller: ProducerId },
    #[error("the host monotonic clock is unavailable")]
    ClockUnavailable,
    #[error("this execution has no simulation attachment")]
    NotAttached,
    #[error("the execution is stopping and refuses new simulation attachment work")]
    Stopping,
    #[error("the simulation attachment is not in the expected Preparing revision")]
    NotPreparing,
    #[error("the simulation attachment is not in the expected Active revision")]
    NotActiveRevision,
    #[error(
        "execution is already attached to world {world} by host {host} and controller {controller}"
    )]
    AlreadyAttached {
        world: crate::model::world::WorldInstanceId,
        host: ProducerId,
        controller: ProducerId,
    },
    #[error("attachment is bound to host {expected}, not request source {observed}")]
    WrongHost {
        expected: ProducerId,
        observed: ProducerId,
    },
}
