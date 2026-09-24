//! Differential-drive body twist to motor shaft velocity, in SI units.
use crate::api::motion::v1::{ActuatorSetpoint, ActuatorTarget, MotionIntent, actuator_target};
use crate::config::MotionConfig;

pub(super) fn stopped_setpoint(config: &MotionConfig) -> ActuatorSetpoint {
    setpoint_from_twist(0.0, 0.0, config)
}

pub(super) fn setpoint_from_intent(
    intent: &MotionIntent,
    config: &MotionConfig,
) -> ActuatorSetpoint {
    let linear = intent
        .linear_x_mps
        .clamp(-config.max_linear_mps, config.max_linear_mps);
    let angular = intent
        .angular_z_radps
        .clamp(-config.max_angular_radps, config.max_angular_radps);
    setpoint_from_twist(linear, angular, config)
}

pub(super) fn setpoint_from_twist(
    linear: f64,
    angular: f64,
    config: &MotionConfig,
) -> ActuatorSetpoint {
    let mut targets = Vec::with_capacity(config.left_wheels.len() + config.right_wheels.len());
    for (wheels, side) in [(&config.left_wheels, -1.0), (&config.right_wheels, 1.0)] {
        let wheel_rate =
            (linear + side * angular * config.wheel_base_m / 2.0) / config.wheel_radius_m;
        targets.extend(wheels.iter().map(|wheel| ActuatorTarget {
            actuator_id: wheel.actuator_id.clone(),
            control: Some(actuator_target::Control::VelocityRadps(
                wheel_rate * wheel.gear_ratio * f64::from(wheel.direction_sign),
            )),
        }));
    }
    ActuatorSetpoint { targets }
}
