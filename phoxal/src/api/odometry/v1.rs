pub const SCHEMA_NAME: &str = "phoxal-api-odometry/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::frame::v1::FrameId;
use crate::api::joint::v1::JointId;
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OdometryEstimate {
    pub pose: PoseEstimate,
    pub velocity: VelocityEstimate,
    pub covariance: Option<Covariance>,
    pub status: Status,
}

impl TypedSchema for OdometryEstimate {
    const SCHEMA_NAME: &'static str = "runtime/odometry/data";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for Status {
    const SCHEMA_NAME: &'static str = "runtime/odometry/status";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for SourceHealth {
    const SCHEMA_NAME: &'static str = "runtime/odometry/debug/source_health";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for Residuals {
    const SCHEMA_NAME: &'static str = "runtime/odometry/debug/residuals";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for Integration {
    const SCHEMA_NAME: &'static str = "runtime/odometry/debug/integration";
    const SCHEMA_VERSION: u32 = 1;
}

crate::bus::topic_leaf! {
    pubsub data {
        path: "runtime/odometry/data",
        payload: OdometryEstimate
    }
}

crate::bus::topic_leaf! {
    pubsub status {
        path: "runtime/odometry/status",
        payload: Status
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub source_health {
            path: "runtime/odometry/debug/source_health",
            payload: SourceHealth
        }
    }

    crate::bus::topic_leaf! {
        pubsub residuals {
            path: "runtime/odometry/debug/residuals",
            payload: Residuals
        }
    }

    crate::bus::topic_leaf! {
        pubsub integration {
            path: "runtime/odometry/debug/integration",
            payload: Integration
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use crate::api::odometry::v1::{
        Integration, OdometryEstimate, Residuals, SourceHealth, Status,
    };

    #[test]
    fn odometry_estimate_schema_is_stable() {
        assert_eq!(OdometryEstimate::SCHEMA_NAME, "runtime/odometry/data");
        assert_eq!(OdometryEstimate::SCHEMA_VERSION, 1);
    }

    #[test]
    fn status_schema_is_stable() {
        assert_eq!(Status::SCHEMA_NAME, "runtime/odometry/status");
        assert_eq!(Status::SCHEMA_VERSION, 1);
    }

    #[test]
    fn source_health_schema_is_stable() {
        assert_eq!(
            SourceHealth::SCHEMA_NAME,
            "runtime/odometry/debug/source_health"
        );
        assert_eq!(SourceHealth::SCHEMA_VERSION, 1);
    }

    #[test]
    fn residuals_schema_is_stable() {
        assert_eq!(Residuals::SCHEMA_NAME, "runtime/odometry/debug/residuals");
        assert_eq!(Residuals::SCHEMA_VERSION, 1);
    }

    #[test]
    fn integration_schema_is_stable() {
        assert_eq!(
            Integration::SCHEMA_NAME,
            "runtime/odometry/debug/integration"
        );
        assert_eq!(Integration::SCHEMA_VERSION, 1);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::data::path(), "runtime/odometry/data");
        assert_eq!(super::status::path(), "runtime/odometry/status");
        assert_eq!(
            super::debug::source_health::path(),
            "runtime/odometry/debug/source_health"
        );
        assert_eq!(
            super::debug::residuals::path(),
            "runtime/odometry/debug/residuals"
        );
        assert_eq!(
            super::debug::integration::path(),
            "runtime/odometry/debug/integration"
        );
    }
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
