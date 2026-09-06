//! Ordered complete world-session projections and a race-closing current query.

crate::endpoints! {
    self: Stream<WorldSessionStateStream, Out>;
    current: Query<WorldSessionStateCurrentRequest, WorldSessionStateCurrentResponse>;
}

use super::{WorldLifecycle, WorldMember, WorldInstanceId, WorldProgress, WorldProvenance};

/// The complete authoritative projection of one world session.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionState {
    pub revision: u64,
    pub instance: WorldInstanceId,
    pub provenance: WorldProvenance,
    pub lifecycle: WorldLifecycle,
    pub progress: WorldProgress,
    /// Current members in strictly increasing `ExecutionId` text order.
    pub members: Vec<WorldMember>,
}

impl WorldSessionState {
    /// Validate ordering and world progress against immutable provenance.
    pub fn validate(&self) -> Result<(), WorldSessionStateError> {
        self.progress
            .validate(self.provenance.time_step_ns)
            .map_err(WorldSessionStateError::Progress)?;
        for member in &self.members {
            member
                .attached_at
                .world
                .validate(self.provenance.time_step_ns)
                .map_err(WorldSessionStateError::AttachmentProgress)?;
            if member.attached_at.world.completed_step() > self.progress.completed_step()
                || member.attached_at.world.elapsed_ns() > self.progress.elapsed_ns()
            {
                return Err(WorldSessionStateError::AttachmentAfterCurrent);
            }
        }
        if self
            .members
            .windows(2)
            .any(|pair| pair[0].execution.to_string() >= pair[1].execution.to_string())
        {
            return Err(WorldSessionStateError::MemberOrder);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorldSessionStateError {
    #[error(transparent)]
    Progress(crate::model::world::WorldProgressError),
    #[error("member attachment progress is invalid: {0}")]
    AttachmentProgress(crate::model::world::WorldProgressError),
    #[error("member attachment progress cannot be ahead of current world progress")]
    AttachmentAfterCurrent,
    #[error("world members must be unique and ordered by ExecutionId")]
    MemberOrder,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionStateStream {
    pub state: WorldSessionState,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionStateCurrentRequest {
    pub instance: WorldInstanceId,
}

/// Identity binding for a long-lived state subscription.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionStateSubscriptionRequest {
    pub instance: WorldInstanceId,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionStateCurrentResponse {
    pub state: WorldSessionState,
}
