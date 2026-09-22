use phoxal::runtime::input::{Commands, Latest};
use phoxal_service_kinematics::OdometryState;
use phoxal_service_world::{WindowRequest, WindowResponse, world};

/// One immutable pose cut for World.
#[phoxal::runtime::inputs]
pub struct WorldInputs {
    /// The latest kinematics-owned odometry, retaining its capture stamp.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub pose: Latest<OdometryState>,
    /// Ordered revision-aware window calls admitted for this invocation.
    #[phoxal::runtime::input(
        port = world::methods::WINDOW.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub windows: Commands<WindowRequest, WindowResponse>,
}
