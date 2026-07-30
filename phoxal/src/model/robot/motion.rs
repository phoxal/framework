//! Canonical robot motion facts normalized from versioned source documents.

use crate::model::component::CapabilityRef;
use crate::model::source::robot::v0 as source;

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
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

impl From<source::CapabilityRef> for CapabilityRef {
    fn from(value: source::CapabilityRef) -> Self {
        Self::new(value.component_id, value.capability_id)
    }
}

impl From<source::MotionLimits> for MotionLimits {
    fn from(value: source::MotionLimits) -> Self {
        Self {
            max_linear_speed_mps: value.max_linear_speed_mps,
            max_angular_speed_radps: value.max_angular_speed_radps,
        }
    }
}

fn convert_refs(values: Vec<source::CapabilityRef>) -> Vec<CapabilityRef> {
    values.into_iter().map(Into::into).collect()
}

impl From<source::KinematicConfig> for KinematicConfig {
    fn from(value: source::KinematicConfig) -> Self {
        match value {
            source::KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                wheel_radius_m,
                wheel_base_m,
            } => Self::Differential {
                left_actuators: convert_refs(left_actuators),
                right_actuators: convert_refs(right_actuators),
                left_encoders: convert_refs(left_encoders),
                right_encoders: convert_refs(right_encoders),
                wheel_radius_m,
                wheel_base_m,
            },
            source::KinematicConfig::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                wheel_radius_m,
                wheel_base_m,
                track_m,
            } => Self::Mecanum {
                front_left_actuator: front_left_actuator.into(),
                front_right_actuator: front_right_actuator.into(),
                rear_left_actuator: rear_left_actuator.into(),
                rear_right_actuator: rear_right_actuator.into(),
                wheel_radius_m,
                wheel_base_m,
                track_m,
            },
            source::KinematicConfig::Ackermann {
                steering_actuator,
                drive_actuator,
                steering_encoder,
                drive_encoder,
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
            } => Self::Ackermann {
                steering_actuator: steering_actuator.into(),
                drive_actuator: drive_actuator.into(),
                steering_encoder: steering_encoder.map(Into::into),
                drive_encoder: drive_encoder.map(Into::into),
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
            },
            source::KinematicConfig::Omnidirectional {
                actuators,
                encoders,
            } => Self::Omnidirectional {
                actuators: convert_refs(actuators),
                encoders: convert_refs(encoders),
            },
        }
    }
}
