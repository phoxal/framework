//! Generated motion messages, typed ports, and domain validation.

use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/phoxal.motion.v1.rs"));

/// Public typed ports owned by the Motion Protobuf service.
pub use motion::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// A motion message violates the version-one domain contract.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// An actuator identity is absent or duplicated.
    #[error("invalid actuator_id: {0}")]
    InvalidActuatorId(&'static str),
    /// A target omits its required control selection.
    #[error("actuator {0} must select exactly one supported control")]
    MissingControl(String),
    /// A selected physical quantity is not finite.
    #[error("actuator {actuator_id} {quantity} must be finite")]
    NonFinite {
        /// The configured actuator identity.
        actuator_id: String,
        /// The invalid quantity.
        quantity: &'static str,
    },
    /// The setpoint membership differs from the configured actuator authority set.
    #[error("setpoint actuator membership does not match the configured authority set")]
    Membership,
}

impl ActuatorSetpoint {
    /// Validates identities, controls, values, and complete configured membership.
    pub fn validate_for<'a>(
        &self,
        required_actuators: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), ValidationError> {
        let required = required_actuators.into_iter().collect::<HashSet<_>>();
        let mut actual = HashSet::with_capacity(self.targets.len());
        for target in &self.targets {
            if target.actuator_id.is_empty() {
                return Err(ValidationError::InvalidActuatorId("empty"));
            }
            if !actual.insert(target.actuator_id.as_str()) {
                return Err(ValidationError::InvalidActuatorId("duplicate"));
            }
            match target.control.as_ref() {
                Some(actuator_target::Control::VelocityRadps(value)) => {
                    validate_finite(&target.actuator_id, *value, "velocity_radps")?;
                }
                Some(actuator_target::Control::TorqueNm(value)) => {
                    validate_finite(&target.actuator_id, *value, "torque_nm")?;
                }
                None => return Err(ValidationError::MissingControl(target.actuator_id.clone())),
            }
        }
        if actual != required {
            return Err(ValidationError::Membership);
        }
        Ok(())
    }
}

fn validate_finite(
    actuator_id: &str,
    value: f64,
    quantity: &'static str,
) -> Result<(), ValidationError> {
    if !value.is_finite() {
        return Err(ValidationError::NonFinite {
            actuator_id: actuator_id.to_owned(),
            quantity,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use phoxal_port::{PortDescriptor, PortKind};
    use prost::Name;

    use super::*;

    #[test]
    fn generated_port_retains_name_kind_and_message_identity() {
        assert_eq!(ports::ACTUATORS.name(), "actuators");
        assert_eq!(
            <phoxal_port::Setpoint<ActuatorSetpoint> as PortDescriptor>::KIND,
            PortKind::Setpoint
        );
        assert_eq!(ActuatorSetpoint::PACKAGE, "phoxal.motion.v1");
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }

    #[test]
    fn validates_zero_targets_and_complete_membership() {
        let setpoint = ActuatorSetpoint {
            targets: vec![
                ActuatorTarget {
                    actuator_id: "left".into(),
                    control: Some(actuator_target::Control::VelocityRadps(0.0)),
                },
                ActuatorTarget {
                    actuator_id: "right".into(),
                    control: Some(actuator_target::Control::TorqueNm(-0.0)),
                },
            ],
        };
        setpoint
            .validate_for(["left", "right"])
            .expect("complete finite setpoint");
    }

    #[test]
    fn rejects_missing_duplicate_and_nonfinite_targets() {
        let duplicate = ActuatorSetpoint {
            targets: vec![
                ActuatorTarget {
                    actuator_id: "left".into(),
                    control: Some(actuator_target::Control::VelocityRadps(1.0)),
                },
                ActuatorTarget {
                    actuator_id: "left".into(),
                    control: Some(actuator_target::Control::VelocityRadps(2.0)),
                },
            ],
        };
        assert_eq!(
            duplicate.validate_for(["left"]),
            Err(ValidationError::InvalidActuatorId("duplicate"))
        );

        let nonfinite = ActuatorSetpoint {
            targets: vec![ActuatorTarget {
                actuator_id: "left".into(),
                control: Some(actuator_target::Control::TorqueNm(f64::INFINITY)),
            }],
        };
        assert!(matches!(
            nonfinite.validate_for(["left"]),
            Err(ValidationError::NonFinite { .. })
        ));
    }
}
