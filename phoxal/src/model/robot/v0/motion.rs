use serde::{Deserialize, Serialize};

use crate::model::component::v0::CapabilityRef;

/// Robot-wide planar motion limits enforced independently by `motion` and
/// `drive`. These are authored facts, not per-runtime configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MotionLimits {
    pub max_linear_speed_mps: f64,
    pub max_angular_speed_radps: f64,
}

impl MotionLimits {
    pub fn validate(self) -> anyhow::Result<Self> {
        anyhow::ensure!(
            self.max_linear_speed_mps.is_finite() && self.max_linear_speed_mps > 0.0,
            "robot.motion_limits.max_linear_speed_mps must be finite and > 0"
        );
        anyhow::ensure!(
            self.max_linear_speed_mps <= f64::from(f32::MAX),
            "robot.motion_limits.max_linear_speed_mps must fit in f32"
        );
        anyhow::ensure!(
            self.max_angular_speed_radps.is_finite() && self.max_angular_speed_radps > 0.0,
            "robot.motion_limits.max_angular_speed_radps must be finite and > 0"
        );
        anyhow::ensure!(
            self.max_angular_speed_radps <= f64::from(f32::MAX),
            "robot.motion_limits.max_angular_speed_radps must fit in f32"
        );
        Ok(self)
    }
}

/// The robot's kinematic model - a direct field of `robot:` (was
/// `motion.kinematic`; the `motion:` wrapper is gone).
///
/// Every `CapabilityRef` field carries a test-only
/// `#[schemars(with = "String")]` override: `CapabilityRef` lives in
/// `crate::model::component::v0` (out of this module's schema-derive scope)
/// and serializes as a plain `"<component>.<capability>"` string
/// (`#[serde(try_from = "String", into = "String")]`), so treating it as
/// `String` in the generated JSON Schema is exact, not an approximation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KinematicConfig {
    Differential {
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        left_actuators: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        right_actuators: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        left_encoders: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        right_encoders: Vec<CapabilityRef>,
        wheel_radius_m: f64,
        wheel_base_m: f64,
    },
    Mecanum {
        #[cfg_attr(test, schemars(with = "String"))]
        front_left_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        front_right_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        rear_left_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        rear_right_actuator: CapabilityRef,
        wheel_radius_m: f64,
        wheel_base_m: f64,
        track_m: f64,
    },
    Ackermann {
        #[cfg_attr(test, schemars(with = "String"))]
        steering_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        drive_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "Option<String>"))]
        steering_encoder: Option<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Option<String>"))]
        drive_encoder: Option<CapabilityRef>,
        wheel_base_m: f64,
        track_m: f64,
        max_steering_angle_rad: f64,
    },
    Omnidirectional {
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        actuators: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        encoders: Vec<CapabilityRef>,
    },
}

impl KinematicConfig {
    /// Stable snake_case label for the kinematic variant; matches the serde
    /// `kind` tag. For diagnostics and conformance evidence only: callers
    /// should pattern-match `KinematicConfig` directly.
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
