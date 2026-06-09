pub const SCHEMA_NAME: &str = "phoxal-api-mission/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::map::v1::MapRevisionId;
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MissionCommand {
    Explore {
        area: Option<AreaHint>,
        completion: ExplorationCompletion,
        max_duration_ns: Option<u64>,
    },
    NavigateTo {
        goal: GoalPose,
        tolerance: GoalTolerance,
        /// Execution budget measured in logical time from goal acceptance.
        ///
        /// Expiry is a mission failure, not mission completion.
        max_duration_ns: Option<u64>,
    },
    Pause,
    Resume,
    Cancel,
    ManualHandover,
}

impl TypedSchema for MissionCommand {
    const SCHEMA_NAME: &'static str = "runtime/mission/command";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AreaHint {
    Polygon {
        frame_id: String,
        map_revision: Option<MapRevisionId>,
        vertices_xy_m: Vec<[f64; 2]>,
    },
    BoundingBox {
        frame_id: String,
        map_revision: Option<MapRevisionId>,
        min_xy_m: [f64; 2],
        max_xy_m: [f64; 2],
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GoalPose {
    Pose2 {
        frame_id: String,
        map_revision: Option<MapRevisionId>,
        xy_m: [f64; 2],
        yaw_rad: f64,
    },
    /// Future profile shape; the v1 planar profile rejects this at validation.
    Pose3 {
        frame_id: String,
        map_revision: Option<MapRevisionId>,
        translation_m: [f64; 3],
        rotation_wxyz: [f64; 4],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GoalTolerance {
    /// Geometric arrival radius in metres.
    pub pos_m: f64,
    /// Optional geometric heading tolerance in radians, compared as a normalized error.
    pub yaw_rad: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplorationCompletion {
    pub mode: ExplorationCompletionMode,
    pub coverage_goal: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationCompletionMode {
    OpenEnded,
    Coverage,
    ReturnToStart,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub mode: MissionMode,
    pub active_goal: Option<Goal>,
    pub failure: Option<MissionFailure>,
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "runtime/mission/state";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionMode {
    Idle,
    Exploring,
    Navigating,
    Paused,
    ManualHandover,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub pose: GoalPose,
    pub tolerance: GoalTolerance,
    /// Execution budget carried with the active goal for inspection on `goal::path()`.
    pub max_duration_ns: Option<u64>,
    pub source: GoalSource,
}

impl TypedSchema for Goal {
    const SCHEMA_NAME: &'static str = "runtime/mission/goal";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalSource {
    Operator,
    Explore,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionFailure {
    pub code: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionTrace {
    pub decisions: Vec<Decision>,
}

impl TypedSchema for DecisionTrace {
    const SCHEMA_NAME: &'static str = "runtime/mission/debug/decision_trace";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub rule: String,
    pub outcome: String,
}

crate::bus::topic_leaf! {
    pubsub command {
        path: "runtime/mission/command",
        payload: MissionCommand
    }
}

crate::bus::topic_leaf! {
    pubsub state {
        path: "runtime/mission/state",
        payload: State
    }
}

crate::bus::topic_leaf! {
    pubsub goal {
        path: "runtime/mission/goal",
        payload: Goal
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub decision_trace {
            path: "runtime/mission/debug/decision_trace",
            payload: DecisionTrace
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{DecisionTrace, Goal, MissionCommand, SCHEMA_NAME, SCHEMA_VERSION, State};

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-mission/v1");
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(MissionCommand::SCHEMA_NAME, "runtime/mission/command");
        assert_eq!(MissionCommand::SCHEMA_VERSION, 1);
        assert_eq!(State::SCHEMA_NAME, "runtime/mission/state");
        assert_eq!(State::SCHEMA_VERSION, 1);
        assert_eq!(Goal::SCHEMA_NAME, "runtime/mission/goal");
        assert_eq!(Goal::SCHEMA_VERSION, 1);
        assert_eq!(
            DecisionTrace::SCHEMA_NAME,
            "runtime/mission/debug/decision_trace"
        );
        assert_eq!(DecisionTrace::SCHEMA_VERSION, 1);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::command::path(), "runtime/mission/command");
        assert_eq!(super::state::path(), "runtime/mission/state");
        assert_eq!(super::goal::path(), "runtime/mission/goal");
        assert_eq!(
            super::debug::decision_trace::path(),
            "runtime/mission/debug/decision_trace"
        );
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-mission/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
