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
