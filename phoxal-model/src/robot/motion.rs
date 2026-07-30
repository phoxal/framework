//! Canonical robot motion facts normalized from versioned source documents.

use crate::ModelError;
use crate::component::CapabilityRef;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MotionLimits {
    pub max_linear_speed_mps: f64,
    pub max_angular_speed_radps: f64,
}

impl MotionLimits {
    pub fn validate(self) -> Result<Self, ModelError> {
        if !(self.max_linear_speed_mps.is_finite()
            && self.max_linear_speed_mps > 0.0
            && self.max_linear_speed_mps <= f64::from(f32::MAX))
        {
            return Err(ModelError::Invalid(
                "motion max_linear_speed_mps must be finite, positive, and fit in f32".into(),
            ));
        }
        if !(self.max_angular_speed_radps.is_finite()
            && self.max_angular_speed_radps > 0.0
            && self.max_angular_speed_radps <= f64::from(f32::MAX))
        {
            return Err(ModelError::Invalid(
                "motion max_angular_speed_radps must be finite, positive, and fit in f32".into(),
            ));
        }
        Ok(self)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
