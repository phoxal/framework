use phoxal::runtime::input::{Commands, Latest};
use phoxal_service_kinematics::OdometryState;
use phoxal_navigation::{ApplyCommandRequest, ApplyCommandResponse, ports};
use phoxal_world::WorldRevision;

/// One immutable input cut for Navigation.
#[phoxal::runtime::inputs]
pub struct NavigationInputs {
    /// Ordered goal and cancellation commands admitted for this invocation.
    #[phoxal::runtime::input(
        port = ports::COMMANDS,
        max_items = 32,
        max_bytes = 16_384
    )]
    pub commands: Commands<ApplyCommandRequest, ApplyCommandResponse>,
    /// The latest measured pose, retaining the provider's capture stamp.
    pub localization: Latest<OdometryState>,
    /// The latest immutable map revision, retaining the provider's capture
    /// stamp.
    pub map: Latest<WorldRevision>,
}
