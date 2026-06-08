use serde::{Deserialize, Serialize};

use phoxal_core_component::v1::CapabilityRef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Motion {
    pub kinematic: KinematicConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KinematicConfig {
    Differential {
        left_actuators: Vec<CapabilityRef>,
        right_actuators: Vec<CapabilityRef>,
        left_encoders: Vec<CapabilityRef>,
        right_encoders: Vec<CapabilityRef>,
        wheel_radius_m: f64,
        wheel_base_m: f64,
    },
    Mecanum {
        front_left_actuator: CapabilityRef,
        front_right_actuator: CapabilityRef,
        rear_left_actuator: CapabilityRef,
        rear_right_actuator: CapabilityRef,
        wheel_radius_m: f64,
        wheel_base_m: f64,
        track_m: f64,
    },
    Ackermann {
        steering_actuator: CapabilityRef,
        drive_actuator: CapabilityRef,
        steering_encoder: Option<CapabilityRef>,
        drive_encoder: Option<CapabilityRef>,
        wheel_base_m: f64,
        track_m: f64,
        max_steering_angle_rad: f64,
    },
    Omnidirectional {
        actuators: Vec<CapabilityRef>,
        encoders: Vec<CapabilityRef>,
    },
}

impl KinematicConfig {
    /// Stable snake_case label for the kinematic variant — matches the serde
    /// `kind` tag. For diagnostics and conformance evidence only; the variant
    /// itself is the single source of truth, so callers pattern-match
    /// `KinematicConfig` directly rather than branching on a parallel enum.
    #[must_use]
    pub const fn variant_label(&self) -> &'static str {
        match self {
            Self::Differential { .. } => "differential",
            Self::Mecanum { .. } => "mecanum",
            Self::Ackermann { .. } => "ackermann",
            Self::Omnidirectional { .. } => "omnidirectional",
        }
    }
}
