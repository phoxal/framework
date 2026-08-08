//! Motor capability: applies `component::motor::Command` to the Webots `Motor`
//! device.
//!
//! A Webots motor is position-controlled by default, so every velocity and
//! torque command first releases the position target to infinity. The declared
//! gear ratio converts the actuator-side command the contract carries into the
//! joint-side value Webots takes.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::capability::MotorCommand;
use phoxal::model::identity::CapabilityRef;

#[derive(Clone, Debug)]
pub(crate) struct MotorSpec {
    pub(crate) reference: CapabilityRef,
    pub(crate) actuator_type: MotorCommand,
    pub(crate) gear_ratio: f64,
}

pub(crate) struct NativeMotor {
    motor: webots_rs::device::motor::Motor,
    actuator_type: MotorCommand,
    gear_ratio: f64,
}

impl NativeMotor {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &MotorSpec) -> Result<Self> {
        Ok(Self {
            motor: webots.motor(spec.reference.to_string())?,
            actuator_type: spec.actuator_type,
            gear_ratio: spec.gear_ratio,
        })
    }

    pub(crate) fn apply(&self, command: &api::component::motor::Command) -> Result<()> {
        match (self.actuator_type, command) {
            (MotorCommand::Velocity, api::component::motor::Command::Velocity(value)) => {
                self.motor.set_position(f64::INFINITY)?;
                self.motor
                    .set_velocity(actuator_to_joint_value(f64::from(*value), self.gear_ratio))?;
            }
            (MotorCommand::Torque, api::component::motor::Command::Torque(value)) => {
                self.motor.set_position(f64::INFINITY)?;
                self.motor
                    .set_torque(actuator_to_joint_value(f64::from(*value), self.gear_ratio))?;
            }
            (MotorCommand::Velocity, api::component::motor::Command::Stop) => {
                self.motor.set_position(f64::INFINITY)?;
                self.motor.set_velocity(0.0)?;
            }
            (MotorCommand::Torque, api::component::motor::Command::Stop) => {
                self.motor.set_position(f64::INFINITY)?;
                self.motor.set_torque(0.0)?;
            }
            (MotorCommand::Position, api::component::motor::Command::Stop) => {
                self.motor.set_velocity(0.0)?;
            }
            (actual, command) => {
                bail!("motor {actual:?} does not support command {command:?}");
            }
        }
        Ok(())
    }
}

fn actuator_to_joint_value(actuator_value: f64, gear_ratio: f64) -> f64 {
    actuator_value / gear_ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actuator_to_joint_scales_by_gear_ratio() {
        assert_eq!(actuator_to_joint_value(4.0, 2.0), 2.0);
    }
}
