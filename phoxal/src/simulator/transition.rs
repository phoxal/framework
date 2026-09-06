//! Live transition and active-boundary correlation.

use super::*;

/// One exact monotonic correlation shared by every output of a native transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveTransitionStamp {
    pub(super) instant: RobotInstant,
    pub(super) world: WorldInstanceId,
    pub(super) revision: u64,
    pub(super) attached_at: LiveAttachmentBoundary,
    pub(super) progress: WorldProgress,
}

/// One current Active attachment boundary for command selection immediately
/// before a native transition.
///
/// This stamp intentionally carries no [`WorldProgress`] and does not
/// implement [`crate::bus::StepStamp`]. It can filter commands and anchor
/// monotonic lease selection, but it cannot publish simulator output or a
/// [`StepEvent`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveBoundaryStamp {
    pub(super) local: LocalInstant,
    pub(super) instant: RobotInstant,
    pub(super) world: WorldInstanceId,
    pub(super) revision: u64,
    pub(super) attached_at: LiveAttachmentBoundary,
}

impl ActiveBoundaryStamp {
    /// The execution's current monotonic robot instant.
    #[must_use]
    pub const fn instant(&self) -> RobotInstant {
        self.instant
    }

    /// The host-monotonic reading captured for lease selection at the same
    /// boundary as [`Self::instant`].
    #[must_use]
    pub const fn local_instant(&self) -> LocalInstant {
        self.local
    }

    #[must_use]
    pub const fn world(&self) -> WorldInstanceId {
        self.world
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn attached_at(&self) -> LiveAttachmentBoundary {
        self.attached_at
    }
}

/// One source-bound host transaction after its ordered Preparing replacement
impl LiveTransitionStamp {
    #[must_use]
    pub const fn instant(&self) -> RobotInstant {
        self.instant
    }

    #[must_use]
    pub const fn world(&self) -> WorldInstanceId {
        self.world
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The immutable progress-to-execution correlation captured when the
    /// execution joined this world.
    #[must_use]
    pub const fn attached_at(&self) -> LiveAttachmentBoundary {
        self.attached_at
    }

    /// The validated world progress completed by this native transition.
    #[must_use]
    pub const fn progress(&self) -> WorldProgress {
        self.progress
    }
}

impl crate::bus::handle::stamp::sealed::Sealed for LiveTransitionStamp {}

impl crate::bus::StepStamp for LiveTransitionStamp {
    fn instant(&self) -> RobotInstant {
        self.instant
    }
}

/// A setpoint receiver that exposes only intent produced under the exact
pub(super) fn validate_next_progress(
    previous: WorldProgress,
    observed: WorldProgress,
) -> Result<(), SimulatorError> {
    let expected =
        previous
            .completed_step()
            .checked_add(1)
            .ok_or(SimulatorError::NonMonotonicProgress {
                previous: previous.completed_step(),
                observed: observed.completed_step(),
            })?;
    if observed.completed_step() != expected || observed.elapsed_ns() <= previous.elapsed_ns() {
        return Err(SimulatorError::NonMonotonicProgress {
            previous: previous.completed_step(),
            observed: observed.completed_step(),
        });
    }
    let time_step_ns = if previous.completed_step() == 0 {
        observed
            .elapsed_ns()
            .checked_sub(previous.elapsed_ns())
            .ok_or(SimulatorError::NonMonotonicProgress {
                previous: previous.completed_step(),
                observed: observed.completed_step(),
            })?
    } else {
        let completed = previous.completed_step();
        previous.elapsed_ns() / completed
    };
    previous.validate(time_step_ns)?;
    observed.validate(time_step_ns)?;
    Ok(())
}
pub(super) fn ensure_live_publication(admitted: bool) -> Result<(), SimulatorError> {
    if admitted {
        Ok(())
    } else {
        Err(SimulatorError::StaleTransition)
    }
}

pub(super) fn admit_step_event(
    publisher: &EventPublisher<StepEvent>,
    bus: &BusHandle,
    transition: &LiveTransitionStamp,
    event: StepEvent,
) -> Result<(), SimulatorError> {
    let admitted = publisher.publish_active_simulation(
        bus.producer(),
        transition.revision,
        transition,
        event,
    )?;
    ensure_live_publication(admitted)
}
