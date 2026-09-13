use phoxal::runtime::input::{Latest, Samples};
use phoxal_motion::MotionStatus;
use phoxal_safety::{RangeSample, WorldBelief, WorldRevision};

/// One immutable input cut for Safety.
#[phoxal::runtime::inputs]
pub struct SafetyInputs {
    /// Latest world belief from the world-owned integration boundary.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub world: Latest<WorldBelief>,
    /// Latest immutable world revision marker.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub world_revision: Latest<WorldRevision>,
    /// Latest motion status evidence. This is health only, not actuator
    /// authority.
    #[phoxal::runtime::input(max_age_ms = 100)]
    pub motion: Latest<MotionStatus>,
    /// Bounded range observations retaining each sensor's capture stamp.
    #[phoxal::runtime::input(max_items = 256, max_bytes = 131_072)]
    pub ranges: Samples<RangeSample>,
}
