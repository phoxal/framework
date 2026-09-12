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
    /// A motion intent owner is absent or too long.
    #[error("owner_id must contain 1 to 64 UTF-8 bytes")]
    InvalidOwner,
    /// A safety reason is malformed or repeated.
    #[error("safety reasons must contain at most four distinct non-empty values")]
    InvalidSafetyReasons,
    /// A required emergency command or decision is absent.
    #[error("{0} is required")]
    Missing(&'static str),
    /// A reset token is malformed.
    #[error("reset_token must contain 1 to 64 UTF-8 bytes")]
    InvalidResetToken,
    /// An emergency response contains an unspecified reason.
    #[error("emergency refusal reason is unspecified or unknown")]
    InvalidEmergencyReason,
    /// A status view contradicts its selected mode.
    #[error("motion status fields are contradictory: {0}")]
    InvalidState(&'static str),
}

impl MotionIntent {
    /// Validates authority identity and finite body-twist values.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.owner_id.is_empty() || self.owner_id.len() > 64 {
            return Err(ValidationError::InvalidOwner);
        }
        if !self.linear_x_mps.is_finite() || !self.angular_z_radps.is_finite() {
            return Err(ValidationError::NonFinite {
                actuator_id: self.owner_id.clone(),
                quantity: "intent",
            });
        }
        Ok(())
    }
}

impl MotionMeasurement {
    /// Validates measured velocities before they enter the motion transition.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.linear_x_mps.is_finite() || !self.angular_z_radps.is_finite() {
            return Err(ValidationError::NonFinite {
                actuator_id: "measurement".to_owned(),
                quantity: "velocity",
            });
        }
        Ok(())
    }
}

impl MotionStatus {
    /// Validates the closed-set mode and authority fields in a status view.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mode = ControlMode::try_from(self.mode)
            .ok()
            .filter(|mode| *mode != ControlMode::Unspecified)
            .ok_or(ValidationError::InvalidEmergencyReason)?;
        if mode == ControlMode::Disarmed && self.selected_owner_id.is_some() {
            return Err(ValidationError::InvalidState(
                "disarmed status has an owner",
            ));
        }
        if let Some(owner) = &self.selected_owner_id
            && (owner.is_empty() || owner.len() > 64)
        {
            return Err(ValidationError::InvalidOwner);
        }
        Ok(())
    }
}

impl SafetyState {
    /// Validates the bounded protective-state reason list.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut reasons = HashSet::with_capacity(self.reasons.len());
        if self.reasons.len() > 4 {
            return Err(ValidationError::InvalidSafetyReasons);
        }
        for reason in &self.reasons {
            if reason.is_empty() || reason.len() > 64 || !reasons.insert(reason) {
                return Err(ValidationError::InvalidSafetyReasons);
            }
        }
        Ok(())
    }
}

impl ApplyEmergencyRequest {
    /// Validates the explicit emergency command and release evidence.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self
            .command
            .as_ref()
            .ok_or(ValidationError::Missing("command"))?
        {
            apply_emergency_request::Command::Engage(_) => Ok(()),
            apply_emergency_request::Command::Release(release) => {
                if release.reset_token.is_empty() || release.reset_token.len() > 64 {
                    Err(ValidationError::InvalidResetToken)
                } else {
                    Ok(())
                }
            }
            apply_emergency_request::Command::Arm(arm) => {
                let mode = ControlMode::try_from(arm.mode)
                    .ok()
                    .filter(|mode| matches!(mode, ControlMode::Manual | ControlMode::Autonomous))
                    .ok_or(ValidationError::InvalidEmergencyReason)?;
                let _ = mode;
                if arm.owner_id.is_empty() || arm.owner_id.len() > 64 {
                    Err(ValidationError::InvalidOwner)
                } else {
                    Ok(())
                }
            }
            apply_emergency_request::Command::Disarm(_) => Ok(()),
        }
    }
}

impl ApplyEmergencyResponse {
    /// Validates an accepted or refused emergency command response.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self
            .decision
            .as_ref()
            .ok_or(ValidationError::Missing("decision"))?
        {
            apply_emergency_response::Decision::Accepted(_) => Ok(()),
            apply_emergency_response::Decision::Refused(refused) => {
                let reason = EmergencyRefusalReason::try_from(refused.reason)
                    .ok()
                    .filter(|reason| *reason != EmergencyRefusalReason::Unspecified)
                    .ok_or(ValidationError::InvalidEmergencyReason)?;
                let _ = reason;
                Ok(())
            }
        }
    }
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
        assert_eq!(ports::MANUAL.name(), "manual");
        assert_eq!(ports::AUTONOMOUS.name(), "autonomous");
        assert_eq!(ports::SAFETY.name(), "safety");
        assert_eq!(ports::MEASUREMENTS.name(), "measurements");
        assert_eq!(ports::STATUS.name(), "status");
        assert_eq!(ports::EMERGENCY.name(), "emergency");
        assert_eq!(
            <phoxal_port::Commands<ApplyEmergencyRequest, ApplyEmergencyResponse> as PortDescriptor>::KIND,
            PortKind::Commands
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

    #[test]
    fn validates_intents_safety_and_emergency_lifecycle_payloads() {
        MotionIntent {
            owner_id: "operator".into(),
            linear_x_mps: 0.2,
            angular_z_radps: -0.1,
        }
        .validate()
        .expect("finite intent");
        SafetyState {
            protective_state_clear: true,
            reasons: vec!["reset-confirmed".into()],
        }
        .validate()
        .expect("bounded safety state");
        let request = ApplyEmergencyRequest {
            command: Some(apply_emergency_request::Command::Release(
                ReleaseEmergency {
                    reset_token: "physical-reset".into(),
                },
            )),
        };
        request.validate().expect("explicit release evidence");
        ApplyEmergencyResponse {
            decision: Some(apply_emergency_response::Decision::Accepted(
                EmergencyAccepted {},
            )),
        }
        .validate()
        .expect("accepted response");
    }

    #[test]
    fn rejects_unbounded_or_ambiguous_motion_inputs() {
        assert!(
            MotionIntent {
                owner_id: String::new(),
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
            }
            .validate()
            .is_err()
        );
        assert!(
            SafetyState {
                protective_state_clear: false,
                reasons: vec!["same".into(), "same".into()],
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            ApplyEmergencyRequest { command: None }.validate(),
            Err(ValidationError::Missing("command"))
        );
        assert_eq!(
            ApplyEmergencyResponse { decision: None }.validate(),
            Err(ValidationError::Missing("decision"))
        );
    }
}
