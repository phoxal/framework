//! The navigation service's payload vocabulary and endpoint contract.
//!
//! Every `phoxal.navigation.v1` message this executable speaks is authored
//! here, private to this binary. The consumed odometry input is the
//! SDK robotics standard; the world inputs are this executable's own typed
//! expectations authored to the published wire identities. Wire identities, field numbers, presence, and the command,
//! decision, and goal-status oneofs match the published Protobuf
//! definitions exactly.
use phoxal::contracts::robotics::OdometryState;

use phoxal::contracts::{Latest, Queue, RequestReply};

/// Navigation's private expectation of the map input: a typed record the
/// robot adapts the producer's revision snapshot into. The owner-qualified
/// identity is derived from this crate and module; no producer's identity
/// is claimed.
#[phoxal::message]
pub struct MapState {
    #[phoxal(tag = 1)]
    pub revision: u64,
    #[phoxal(tag = 2)]
    pub available: bool,
    /// Oldest source capture supporting this state, in the execution timeline.
    /// Republishing or deriving state must preserve this time.
    #[phoxal(tag = 3)]
    pub oldest_capture_time_nanos: Option<u64>,
}

#[phoxal::messages(package = "phoxal.navigation.v1")]
mod v1 {
    use super::{Latest, MapState, OdometryState, Queue, RequestReply};

    /// A goal's target pose.
    pub struct GoalTarget {
        #[phoxal(tag = 1)]
        pub frame_id: String,
        #[phoxal(tag = 2)]
        pub x_m: f64,
        #[phoxal(tag = 3)]
        pub y_m: f64,
        #[phoxal(tag = 4)]
        pub final_heading_rad: Option<f64>,
    }

    /// The start-goal command payload.
    pub struct StartGoal {
        #[phoxal(tag = 1)]
        pub goal_id: String,
        #[phoxal(tag = 2)]
        pub target: Option<GoalTarget>,
    }

    /// The cancel-goal command payload.
    pub struct CancelGoal {
        #[phoxal(tag = 1)]
        pub goal_id: String,
    }

    /// One navigation command: the apply-command request itself.
    pub enum ApplyCommand {
        #[phoxal(tag = 1)]
        Start(StartGoal),
        #[phoxal(tag = 2)]
        Cancel(CancelGoal),
    }

    /// Why a command was refused.
    pub enum RefusalReason {
        Unspecified = 0,
        InvalidGoal = 1,
        UnknownGoal = 2,
        ConflictingGoal = 3,
        Unavailable = 4,
    }

    /// Why navigation is unavailable.
    pub enum UnavailableReason {
        Unspecified = 0,
        InputsNotReady = 1,
        Localization = 2,
        Map = 3,
        Motion = 4,
    }

    /// The refused variant of an apply-command response.
    pub struct Refused {
        #[phoxal(tag = 1)]
        pub reason: RefusalReason,
        #[phoxal(tag = 2)]
        pub unavailable_reasons: Vec<UnavailableReason>,
    }

    /// One apply-command response.
    pub enum ApplyCommandResponse {
        #[phoxal(tag = 1)]
        Accepted,
        #[phoxal(tag = 2)]
        Refused(Refused),
    }

    /// The navigation product's phase.
    pub enum Phase {
        Unspecified = 0,
        Idle = 1,
        Searching = 2,
        Following = 3,
    }

    /// The navigation product's state snapshot.
    pub struct NavigationState {
        #[phoxal(tag = 1)]
        pub phase: Phase,
        #[phoxal(tag = 2)]
        pub active_goal_id: Option<String>,
        #[phoxal(tag = 3)]
        pub map_revision: Option<u64>,
        #[phoxal(tag = 4)]
        pub unavailable_reasons: Vec<UnavailableReason>,
    }

    /// How a goal ended.
    pub enum GoalOutcome {
        Unspecified = 0,
        Reached = 1,
        Cancelled = 2,
        Replaced = 3,
        NoPath = 4,
        Unavailable = 5,
    }

    /// One finished goal notification.
    pub struct GoalFinished {
        #[phoxal(tag = 1)]
        pub goal_id: String,
        #[phoxal(tag = 2)]
        pub outcome: GoalOutcome,
        #[phoxal(tag = 3)]
        pub unavailable_reasons: Vec<UnavailableReason>,
    }

    /// One goal-status request.
    pub struct GetGoalStatusRequest {
        #[phoxal(tag = 1)]
        pub goal_id: String,
    }

    /// The running variant of a goal-status response.
    pub struct GoalRunning {
        #[phoxal(tag = 1)]
        pub goal_id: String,
    }

    /// The unknown-or-no-longer-retained variant of a goal-status response.
    pub struct GoalUnknownOrNoLongerRetained {
        #[phoxal(tag = 1)]
        pub goal_id: String,
    }

    /// One goal-status response.
    pub enum GetGoalStatusResponse {
        #[phoxal(tag = 1)]
        Running(GoalRunning),
        #[phoxal(tag = 2)]
        Finished(GoalFinished),
        #[phoxal(tag = 3)]
        UnknownOrNoLongerRetained(GoalUnknownOrNoLongerRetained),
    }

    /// The navigation service's endpoint contract.
    #[phoxal::endpoints]
    pub struct NavigationApi {
        #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
        localization: Latest<OdometryState>,

        #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
        map: Latest<MapState>,

        #[phoxal::output(max_items = 64, max_bytes = 16_384)]
        finished: Queue<GoalFinished>,

        #[phoxal::output(projection = state, bootstrap, max_bytes = 1024)]
        status: Latest<NavigationState>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        apply_command: RequestReply<ApplyCommand, ApplyCommandResponse>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        get_goal_status: RequestReply<GetGoalStatusRequest, GetGoalStatusResponse>,
    }
}

pub use v1::*;
