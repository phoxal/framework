//! Cross-process plan records exchanged by the Webots host and controllers.
//!
//! The host derives and validates these records. This crate intentionally owns
//! only their stable wire representation.

use phoxal::model::asset::AssetId;
use phoxal::model::component::capability::{CapabilityKind, MotorCommand};
use phoxal::model::identity::{CapabilityRef, ComponentInstanceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotSimulationPlan {
    pub robot: String,
    pub basic_time_step_ms: i32,
    pub substitutions: Vec<DriverSubstitution>,
    pub capabilities: Vec<CapabilityBinding>,
    pub links: Vec<LinkSimulation>,
    pub assets: Vec<PlannedAsset>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverSubstitution {
    pub participant: ComponentInstanceId,
    pub capabilities: Vec<CapabilityRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilityBinding {
    Motor {
        reference: CapabilityRef,
        native_device: String,
        target: PlannedTarget,
        command: MotorCommand,
    },
    Encoder {
        reference: CapabilityRef,
        native_device: String,
        target: PlannedTarget,
        sampling: SamplingPlan,
    },
    Sampled {
        reference: CapabilityRef,
        native_device: String,
        target: PlannedTarget,
        capability: SampledCapabilityKind,
        sampling: SamplingPlan,
    },
}

impl CapabilityBinding {
    #[must_use]
    pub fn reference(&self) -> &CapabilityRef {
        match self {
            Self::Motor { reference, .. }
            | Self::Encoder { reference, .. }
            | Self::Sampled { reference, .. } => reference,
        }
    }
    #[must_use]
    pub fn native_device(&self) -> &str {
        match self {
            Self::Motor { native_device, .. }
            | Self::Encoder { native_device, .. }
            | Self::Sampled { native_device, .. } => native_device,
        }
    }
    #[must_use]
    pub fn target(&self) -> &PlannedTarget {
        match self {
            Self::Motor { target, .. }
            | Self::Encoder { target, .. }
            | Self::Sampled { target, .. } => target,
        }
    }
    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        match self {
            Self::Motor { .. } => CapabilityKind::Motor,
            Self::Encoder { .. } => CapabilityKind::Encoder,
            Self::Sampled { capability, .. } => capability.capability_kind(),
        }
    }
    #[must_use]
    pub fn sampling(&self) -> Option<&SamplingPlan> {
        match self {
            Self::Encoder { sampling, .. } | Self::Sampled { sampling, .. } => Some(sampling),
            Self::Motor { .. } => None,
        }
    }
    #[must_use]
    pub const fn motor_command(&self) -> Option<MotorCommand> {
        match self {
            Self::Motor { command, .. } => Some(*command),
            Self::Encoder { .. } | Self::Sampled { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampledCapabilityKind {
    Accelerometer,
    Gyroscope,
    Imu,
    Gnss,
    Camera,
    Depth,
    Range,
}

impl SampledCapabilityKind {
    #[must_use]
    pub const fn capability_kind(self) -> CapabilityKind {
        match self {
            Self::Accelerometer => CapabilityKind::Accelerometer,
            Self::Gyroscope => CapabilityKind::Gyroscope,
            Self::Imu => CapabilityKind::Imu,
            Self::Gnss => CapabilityKind::Gnss,
            Self::Camera => CapabilityKind::Camera,
            Self::Depth => CapabilityKind::Depth,
            Self::Range => CapabilityKind::Range,
        }
    }
    #[must_use]
    pub const fn from_capability_kind(kind: CapabilityKind) -> Option<Self> {
        Some(match kind {
            CapabilityKind::Accelerometer => Self::Accelerometer,
            CapabilityKind::Gyroscope => Self::Gyroscope,
            CapabilityKind::Imu => Self::Imu,
            CapabilityKind::Gnss => Self::Gnss,
            CapabilityKind::Camera => Self::Camera,
            CapabilityKind::Depth => Self::Depth,
            CapabilityKind::Range => Self::Range,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlannedTarget {
    Link { id: String },
    Joint { id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingPlan {
    pub publish_rate_hz: f64,
    pub native_sampling_rate_hz: f64,
    pub native_period_ms: i32,
    pub publish_period_ns: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSimulation {
    pub component: ComponentInstanceId,
    pub link: String,
    pub contact_material: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedAsset {
    pub id: AssetId,
    pub bytes: u64,
    pub sha256: String,
}
