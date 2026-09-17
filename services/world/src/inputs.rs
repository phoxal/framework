use phoxal::runtime::input::Latest;
use phoxal_service_kinematics::OdometryState;

/// One immutable pose cut for World.
#[phoxal::runtime::inputs]
pub struct WorldInputs {
    /// The latest kinematics-owned odometry, retaining its capture stamp.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub pose: Latest<OdometryState>,
}
