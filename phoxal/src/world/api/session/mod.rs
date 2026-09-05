//! Complete world-session state, diagnostics, and explicit operations.

crate::nodes! {
    state;
    diagnostics;
    control;
    connect;
}

/// Durable backend-neutral documents shared by a local world host and client.
pub mod document;

pub use crate::model::identity::{SpawnId, WorldId};
pub use crate::model::structure::Pose;
pub use crate::model::world::{
    LiveAttachmentBoundary, WorldDigest, WorldInstanceId, WorldProgress, WorldProvenance,
};

use crate::identity::{ExecutionId, ProducerId, RobotId};
use crate::supervisor::api::simulation::SimulationEndReason;

/// Whether a Ready Live world is paused or requesting native REAL_TIME motion.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldMotion {
    Paused,
    Running,
}

/// One non-contradictory world-session lifecycle.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldLifecycle {
    Starting,
    Ready { motion: WorldMotion },
    Stopping,
    Failed { reason: SimulationEndReason },
}

/// The current attachment phase of one robot member.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldMemberPhase {
    Preparing,
    Active,
    Removing,
}

/// One current robot member, keyed and ordered by execution identity.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldMember {
    pub execution: ExecutionId,
    pub robot: RobotId,
    pub controller: ProducerId,
    pub phase: WorldMemberPhase,
    pub attached_at: LiveAttachmentBoundary,
    /// The resolved authored spawn, including automatic single-spawn selection.
    pub spawn: SpawnId,
    pub initial_pose: Pose,
}

/// Why one member left a world that may remain live for other robots.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldMemberEndReason {
    Stopped,
    SupervisorLost,
    ControllerFault,
    AttachmentFailed,
}

/// Whether member cleanup completed without residue.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldMemberCleanup {
    Complete,
    Incomplete { detail: String },
}

/// Persistable terminal evidence for one former member.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldMemberTerminal {
    pub execution: ExecutionId,
    pub robot: RobotId,
    pub controller: ProducerId,
    pub spawn: SpawnId,
    pub reason: WorldMemberEndReason,
    pub last_progress: WorldProgress,
    pub cleanup: WorldMemberCleanup,
    pub evidence_paths: Vec<String>,
}
