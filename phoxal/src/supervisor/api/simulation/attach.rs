//! Begin one source-bound Live attachment transaction.

crate::endpoints! {
    self: Query<AttachRequest, AttachResponse>;
}

use super::{SimulationAttachmentState, WorldInstanceId, WorldProgress};
use crate::identity::ProducerId;
use crate::supervisor::api::time_domain::TimeDomain;

/// The host proposal for one already prepared per-Robot controller.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct AttachRequest {
    world: WorldInstanceId,
    controller: ProducerId,
    progress: WorldProgress,
}

impl AttachRequest {
    /// Build a host-attributed request only after validating the boundary
    /// against the session's immutable physics quantum.
    ///
    /// # Errors
    ///
    /// Returns [`crate::model::world::WorldProgressError`] when the completed
    /// step and elapsed duration do not describe the same world boundary.
    pub fn validated(
        world: WorldInstanceId,
        controller: ProducerId,
        progress: WorldProgress,
        time_step_ns: u64,
    ) -> Result<Self, crate::model::world::WorldProgressError> {
        progress.validate(time_step_ns)?;
        Ok(Self {
            world,
            controller,
            progress,
        })
    }

    /// The independently hosted world this execution will join.
    #[must_use]
    pub const fn world(self) -> WorldInstanceId {
        self.world
    }

    /// The exact external producer delegated to simulate this Robot.
    #[must_use]
    pub const fn controller(self) -> ProducerId {
        self.controller
    }

    /// The validated world boundary captured by the host.
    #[must_use]
    pub const fn progress(self) -> WorldProgress {
        self.progress
    }
}

/// The committed Active binding and unchanged monotonic execution domain.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct AttachResponse {
    pub attachment: SimulationAttachmentState,
    pub time_domain: TimeDomain,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn producer() -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | 7).expect("a canonical producer")
    }

    #[test]
    fn malformed_progress_cannot_be_decoded_into_an_attach_request() {
        let request = serde_json::from_value::<AttachRequest>(serde_json::json!({
            "world": WorldInstanceId::mint(),
            "controller": producer(),
            "progress": {
            "completed_step": 3,
            "elapsed_ns": 35,
            }
        }));

        assert!(matches!(
            request,
            Err(error) if error.to_string().contains("positive integral physics quantum")
        ));
    }
}
