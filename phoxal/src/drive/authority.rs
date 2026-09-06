//! Authority policy for the framework's built-in drive service.

use std::time::Duration;

use crate::bus::FixedSourceLease;
use crate::identity::ParticipantId;
use crate::model::Robot;
use crate::model::identity::CapabilityRef;

const COMMAND_SILENCE: Duration = Duration::from_millis(150);

/// The fixed source authorized to provide built-in drive motor commands.
#[derive(Clone, Debug)]
pub struct DriveCommandAuthority {
    source: ParticipantId,
}

/// A motor is not present exactly once in the compiled drive topology.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "motor '{capability}' must occur exactly once in the compiled kinematic actuator topology for the built-in drive authority; found {count} occurrences"
)]
pub struct DriveAuthorityError {
    pub capability: CapabilityRef,
    pub count: usize,
}

impl DriveCommandAuthority {
    /// Construct the fixed authority represented by the built-in drive service.
    pub fn standard() -> Result<Self, crate::identity::ParticipantIdError> {
        Ok(Self {
            source: ParticipantId::new("drive")?,
        })
    }

    #[must_use]
    pub const fn silence() -> Duration {
        COMMAND_SILENCE
    }

    #[must_use]
    pub fn source(&self) -> &ParticipantId {
        &self.source
    }

    #[must_use]
    pub fn motor_lease(&self) -> FixedSourceLease<crate::api::component::motor::Command> {
        FixedSourceLease::new(
            "component/motor/command",
            self.source.clone(),
            COMMAND_SILENCE,
            Duration::MAX,
        )
    }

    /// Verify that this framework authority may command one compiled motor.
    pub fn validate_motor(
        robot: &Robot,
        capability: &CapabilityRef,
    ) -> Result<(), DriveAuthorityError> {
        let count = robot.motion().kinematic().actuator_occurrences(capability);
        if count == 1 {
            Ok(())
        } else {
            Err(DriveAuthorityError {
                capability: capability.clone(),
                count,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::component::motor::Command;
    use crate::bus::{
        LeaseDecision, LocalInstant, ParticipantReadyStatus, ParticipantSourceIdentity,
    };
    use crate::identity::ProducerId;

    #[test]
    fn paused_drive_intent_expires_by_host_time_and_a_later_command_stays_fresh() {
        let authority = DriveCommandAuthority::standard().expect("drive authority");
        let producer = ProducerId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001)
            .expect("canonical producer");
        let source = ParticipantSourceIdentity::new(authority.source().clone(), producer);
        let mut lease = authority.motor_lease();
        lease.update_ready(&source, ParticipantReadyStatus::Ready);
        let paused_at = LocalInstant::from_boot_ns(1_000_000_000);
        assert_eq!(
            lease.offer(Some(&source), 1, paused_at, Command::Velocity(1.0)),
            LeaseDecision::Acquired
        );
        assert!(
            lease
                .live_host(paused_at.saturating_add(DriveCommandAuthority::silence()))
                .is_none()
        );

        let later = paused_at.saturating_add(Duration::from_millis(250));
        assert_eq!(
            lease.offer(Some(&source), 2, later, Command::Velocity(2.0)),
            LeaseDecision::Renewed
        );
        assert!(
            lease
                .live_host(later.saturating_add(Duration::from_millis(149)))
                .is_some()
        );
        lease.update_ready(&source, ParticipantReadyStatus::Lost);
        assert!(lease.live_host(later).is_none());
    }
}
