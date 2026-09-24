use crate::api::__contracts::phoxal::kinematics::v1::OdometryState;
use crate::api::__contracts::phoxal::world::v1::WorldRevision;
use crate::api::navigation::v1::{
    ApplyCommandRequest, ApplyCommandResponse, GetGoalStatusRequest, GetGoalStatusResponse,
    navigation,
};
use phoxal::runtime::input::{Commands, Latest};

/// One immutable input cut for Navigation.
#[phoxal::runtime::inputs]
pub struct NavigationInputs {
    /// Ordered goal and cancellation commands admitted for this invocation.
    #[phoxal::runtime::input(
        port = navigation::methods::APPLY_COMMAND.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub commands: Commands<ApplyCommandRequest, ApplyCommandResponse>,
    /// Ordered status calls share the Navigation service admission order.
    #[phoxal::runtime::input(
        port = navigation::methods::GET_GOAL_STATUS.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub status_calls: Commands<GetGoalStatusRequest, GetGoalStatusResponse>,
    /// The latest measured pose, retaining the provider's capture stamp.
    pub localization: Latest<OdometryState>,
    /// The latest immutable map revision, retaining the provider's capture
    /// stamp.
    pub map: Latest<WorldRevision>,
}
