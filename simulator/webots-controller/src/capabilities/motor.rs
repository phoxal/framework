//! Motor capability: subscribes `component::motor::Command` and drives the
//! Webots `Motor` device. Moved from the monolith's `MotorSpec` (main.rs:556-561),
//! `NativeMotor` (main.rs:1077-1140), and the actuator/joint conversion helper
//! (main.rs:1498-1501).

use anyhow::{Result, anyhow, bail};
use phoxal::api;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::component::v0::capability::MotorCommand;

#[derive(Clone, Debug)]
pub(crate) struct MotorSpec {
    pub(crate) reference: CapabilityRef,
    pub(crate) actuator_type: MotorCommand,
    pub(crate) gear_ratio: f64,
}

pub(crate) struct NativeMotor {
    pub(crate) reference: CapabilityRef,
    motor: webots_rs::device::motor::Motor,
    actuator_type: MotorCommand,
    gear_ratio: f64,
}

impl NativeMotor {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &MotorSpec) -> Result<Self> {
        let motor = webots
            .motor(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            reference: spec.reference.clone(),
            motor,
            actuator_type: spec.actuator_type,
            gear_ratio: spec.gear_ratio,
        })
    }

    pub(crate) fn apply(&self, command: &api::component::motor::Command) -> Result<()> {
        match (self.actuator_type, command) {
            (MotorCommand::Velocity, api::component::motor::Command::Velocity(value)) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor
                    .set_velocity(actuator_to_joint_value(f64::from(*value), self.gear_ratio))
                    .map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Torque, api::component::motor::Command::Torque(value)) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor
                    .set_torque(actuator_to_joint_value(f64::from(*value), self.gear_ratio))
                    .map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Velocity, api::component::motor::Command::Stop) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor
                    .set_velocity(0.0)
                    .map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Torque, api::component::motor::Command::Stop) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor.set_torque(0.0).map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Position, api::component::motor::Command::Stop) => {
                self.motor
                    .set_velocity(0.0)
                    .map_err(|error| anyhow!(error))?;
            }
            (actual, command) => {
                bail!("motor {actual:?} does not support command {command:?}");
            }
        }
        Ok(())
    }
}

pub(crate) fn actuator_to_joint_value(actuator_value: f64, gear_ratio: f64) -> f64 {
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
