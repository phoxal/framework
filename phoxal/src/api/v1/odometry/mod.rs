pub const SCHEMA_NAME: &str = "phoxal-api-odometry/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::v1::frame::FrameId;
use crate::api::v1::joint::JointId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OdometryEstimate {
    pub pose: PoseEstimate,
    pub velocity: VelocityEstimate,
    pub covariance: Option<Covariance>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseEstimate {
    pub frame_id: FrameId,
    pub child_frame_id: FrameId,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VelocityEstimate {
    pub frame_id: FrameId,
    pub linear_mps: [f64; 3],
    pub angular_radps: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Covariance {
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub mode: StatusMode,
    pub reasons: Vec<StatusReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusMode {
    Initializing,
    Tracking,
    Degraded,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatusReason {
    /// A required joint stream is missing or outside its freshness window.
    JointStale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHealth {
    pub sources: Vec<SourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source_id: SourceId,
    pub healthy: bool,
    pub reason: Option<SourceReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceId {
    /// Joint position stream from the joint runtime.
    Joint(JointId),
    /// IMU stream when odometry fuses body attitude.
    Imu,
    /// Raw encoder stream when odometry consumes encoder data directly.
    Encoder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceReason {
    /// The source produced data before but is outside its freshness window.
    Stale,
    /// The source has not produced the sample required for tracking.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Residuals {
    pub residuals: Vec<Residual>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Residual {
    pub source_id: SourceId,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Integration {
    pub steps: Vec<IntegrationStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationStep {
    pub source_id: SourceId,
    pub delta_pose_m: [f64; 3],
    pub delta_yaw_rad: f64,
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-odometry/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
