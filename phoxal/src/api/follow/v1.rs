use crate::api::localize::v1::LocalizationRevisionId;
use crate::api::map::v1::MapRevisionId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: String,
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub status: FollowStatus,
    pub reason: Option<FollowReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowStatus {
    Idle,
    Tracking,
    Paused,
    Refused,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowReason {
    NoLocalizationState,
    LocalizationInitializing,
    LocalizationLost,
    LocalizationRelocalizing,
    UnsupportedLocalizationMode,
    PathLocalizeRevisionMismatch,
    LocalizationRevisionUnknown,
    NoLocalizationPose,
    Arrived,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackingError {
    pub lateral_m: f64,
    pub heading_rad: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidates {
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Costs {
    pub costs: Vec<Cost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionInputs {
    pub map_revision: Option<MapRevisionId>,
    pub localization_revision: Option<LocalizationRevisionId>,
}
