use crate::webots::controller::ActuatorType;
use phoxal::api::component::capability::motor::v1::Command;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub actuator_type: ActuatorType,
    pub gear_ratio: f64,
}

pub struct Motor {
    motor: webots_rs::device::motor::Motor,
    gear_ratio: f64,
    actuator_type: ActuatorType,
}

impl Motor {
    pub fn new(motor: webots_rs::device::motor::Motor, config: &Config) -> anyhow::Result<Self> {
        if config.gear_ratio <= f64::EPSILON {
            anyhow::bail!("gear_ratio must be > 0");
        }

        Ok(Self {
            motor,
            gear_ratio: config.gear_ratio,
            actuator_type: config.actuator_type,
        })
    }

    pub fn apply(&self, command: &Command) -> anyhow::Result<()> {
        match command {
            Command::Velocity(velocity) => {
                self.ensure_supported(ActuatorType::Velocity)?;
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow::anyhow!(error))?;
                self.motor
                    .set_velocity(self.to_joint_value(f64::from(*velocity)))
                    .map_err(|error| anyhow::anyhow!(error))?;
            }
            Command::Position(position) => {
                self.ensure_supported(ActuatorType::Position)?;
                self.motor
                    .set_position(self.to_joint_value(f64::from(*position)))
                    .map_err(|error| anyhow::anyhow!(error))?;
            }
            Command::Torque(torque) => {
                self.ensure_supported(ActuatorType::Torque)?;
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow::anyhow!(error))?;
                self.motor
                    .set_torque(self.to_joint_value(f64::from(*torque)))
                    .map_err(|error| anyhow::anyhow!(error))?;
            }
        }

        Ok(())
    }

    fn ensure_supported(&self, expected: ActuatorType) -> anyhow::Result<()> {
        if self.actuator_type == expected {
            Ok(())
        } else {
            anyhow::bail!(
                "received {:?} command for {:?} motor",
                expected,
                self.actuator_type
            )
        }
    }

    fn to_joint_value(&self, actuator_value: f64) -> f64 {
        actuator_to_joint_value(actuator_value, self.gear_ratio)
    }
}

fn actuator_to_joint_value(actuator_value: f64, gear_ratio: f64) -> f64 {
    // Incoming motor commands are already actuator-space commands.
    // The drive runtime has already applied model motor.direction_sign.
    actuator_value / gear_ratio
}

#[cfg(test)]
mod tests {
    use super::actuator_to_joint_value;

    #[test]
    fn actuator_commands_pass_through_without_extra_direction_inversion() {
        assert_eq!(actuator_to_joint_value(-4.0, 2.0), -2.0);
        assert_eq!(actuator_to_joint_value(4.0, 2.0), 2.0);
    }
}
