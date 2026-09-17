//! Generated motion messages, typed ports, and domain validation.
//!
//! Motion owns the protective-constraint payload
//! (`ConstraintReason`, `Constraint`, `Permission`, `MotionConstraints`) after
//! the protective-constraint ownership change. The pure validation logic for
//! that payload therefore lives here, alongside the actuator validation. Safety
//! imports these types through its `extern_path` mapping and adds its own
//! assessment on top without redefining the payload.

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

/// A protective-constraint payload violates the version-one domain contract.
///
/// The constraint payload lives in the Motion owner because Motion is the
/// final actuator authority; payload validation is part of the Motion boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConstraintValidationError {
    /// A constraint has an invalid reason or limit shape.
    #[error("safety constraint is invalid")]
    InvalidConstraint,
    /// A constraints product has contradictory permission or validity.
    #[error("safety constraints are incoherent")]
    InvalidProduct,
}

impl Constraint {
    /// Validates one protective constraint's finite optional limits.
    pub fn validate(&self) -> Result<(), ConstraintValidationError> {
        let reason = ConstraintReason::try_from(self.reason)
            .ok()
            .filter(|reason| *reason != ConstraintReason::Unspecified)
            .ok_or(ConstraintValidationError::InvalidConstraint)?;
        let _ = reason;
        for value in [
            self.max_linear_speed_mps,
            self.max_angular_speed_radps,
            self.observed_value,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0.0 {
                return Err(ConstraintValidationError::InvalidConstraint);
            }
        }
        if matches!(
            reason,
            ConstraintReason::ObstacleProximity
                | ConstraintReason::RangeUnavailable
                | ConstraintReason::RangeFault
                | ConstraintReason::WorldUnavailable
                | ConstraintReason::LocalizationUncertain
                | ConstraintReason::MapUnavailable
                | ConstraintReason::MapBlocked
                | ConstraintReason::MotionUnavailable
                | ConstraintReason::MotionFault
        ) && self.max_linear_speed_mps.is_none()
            && self.max_angular_speed_radps.is_none()
            && self.observed_value.is_none()
        {
            return Err(ConstraintValidationError::InvalidConstraint);
        }
        Ok(())
    }
}

impl MotionConstraints {
    /// Validates permission, bounded reasons, the expiry interval, and the
    /// fail-closed invariants around Motion's protective evidence.
    pub fn validate(&self) -> Result<(), ConstraintValidationError> {
        let permission = Permission::try_from(self.permission)
            .ok()
            .filter(|permission| *permission != Permission::Unspecified)
            .ok_or(ConstraintValidationError::InvalidProduct)?;
        if permission != Permission::Stopped
            && self
                .oldest_capture_time_nanos
                .is_none_or(|capture| capture > self.valid_from_nanos)
        {
            return Err(ConstraintValidationError::InvalidProduct);
        }
        if self.expires_at_nanos < self.valid_from_nanos {
            return Err(ConstraintValidationError::InvalidProduct);
        }
        if self.constraints.len() > 16 {
            return Err(ConstraintValidationError::InvalidProduct);
        }
        let mut reasons = HashSet::with_capacity(self.constraints.len());
        let mut has_limit = false;
        for constraint in &self.constraints {
            constraint.validate()?;
            let reason = ConstraintReason::try_from(constraint.reason)
                .map_err(|_| ConstraintValidationError::InvalidConstraint)?;
            if !reasons.insert(reason) {
                return Err(ConstraintValidationError::InvalidProduct);
            }
            if permission == Permission::Limited
                && (reason != ConstraintReason::ObstacleProximity
                    || (constraint.max_linear_speed_mps.is_none()
                        && constraint.max_angular_speed_radps.is_none()))
            {
                return Err(ConstraintValidationError::InvalidProduct);
            }
            has_limit |= constraint.max_linear_speed_mps.is_some()
                || constraint.max_angular_speed_radps.is_some();
        }
        match permission {
            Permission::Clear if !self.constraints.is_empty() => {
                Err(ConstraintValidationError::InvalidProduct)
            }
            Permission::Limited if self.constraints.is_empty() || !has_limit => {
                Err(ConstraintValidationError::InvalidProduct)
            }
            Permission::Stopped if self.constraints.is_empty() => {
                Err(ConstraintValidationError::InvalidProduct)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use phoxal::port::{PortDescriptor, PortKind};
    use prost::Name;

    use super::*;

    #[test]
    fn generated_port_retains_name_kind_and_message_identity() {
        assert_eq!(ports::ACTUATORS.name(), "actuators");
        assert_eq!(
            <phoxal::port::Setpoint<ActuatorSetpoint> as PortDescriptor>::KIND,
            PortKind::Setpoint
        );
        assert_eq!(ports::MANUAL.name(), "manual");
        assert_eq!(ports::AUTONOMOUS.name(), "autonomous");
        assert_eq!(ports::STATUS.name(), "status");
        assert_eq!(ports::EMERGENCY.name(), "emergency");
        assert_eq!(
            <phoxal::port::Commands<ApplyEmergencyRequest, ApplyEmergencyResponse> as PortDescriptor>::KIND,
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
        assert_eq!(
            ApplyEmergencyRequest { command: None }.validate(),
            Err(ValidationError::Missing("command"))
        );
        assert_eq!(
            ApplyEmergencyResponse { decision: None }.validate(),
            Err(ValidationError::Missing("decision"))
        );
    }

    #[test]
    fn constraint_validation_matches_the_published_contract() {
        MotionConstraints {
            sequence: 1,
            permission: Permission::Clear as i32,
            constraints: Vec::new(),
            oldest_capture_time_nanos: Some(0),
            valid_from_nanos: 10,
            expires_at_nanos: 20,
        }
        .validate()
        .expect("clear has no constraints");
        let bad = MotionConstraints {
            sequence: 1,
            permission: Permission::Clear as i32,
            constraints: vec![Constraint {
                reason: ConstraintReason::MapUnavailable as i32,
                max_linear_speed_mps: None,
                max_angular_speed_radps: None,
                observed_value: Some(0.0),
            }],
            oldest_capture_time_nanos: Some(0),
            valid_from_nanos: 10,
            expires_at_nanos: 20,
        };
        assert_eq!(
            bad.validate(),
            Err(ConstraintValidationError::InvalidProduct)
        );
    }

    #[test]
    fn rejects_stale_or_expired_protective_evidence() {
        // Missing constraint with a non-stopped permission is fail-closed.
        let missing = MotionConstraints {
            sequence: 1,
            permission: Permission::Limited as i32,
            constraints: Vec::new(),
            oldest_capture_time_nanos: Some(0),
            valid_from_nanos: 10,
            expires_at_nanos: 20,
        };
        assert_eq!(
            missing.validate(),
            Err(ConstraintValidationError::InvalidProduct)
        );
        // Future-dated capture provenance (capture > valid_from) is rejected
        // for non-stopped permissions. Clear-with-empty is the only
        // permission that validates without constraints, so use it to
        // exercise the capture-time check in isolation.
        let future_capture = MotionConstraints {
            sequence: 1,
            permission: Permission::Clear as i32,
            constraints: Vec::new(),
            oldest_capture_time_nanos: Some(10),
            valid_from_nanos: 5,
            expires_at_nanos: 20,
        };
        assert_eq!(
            future_capture.validate(),
            Err(ConstraintValidationError::InvalidProduct)
        );
        // Future-dated capture provenance is also rejected for Limited
        // payloads that carry a constraint.
        let future_capture_limited = MotionConstraints {
            sequence: 1,
            permission: Permission::Limited as i32,
            constraints: vec![Constraint {
                reason: ConstraintReason::ObstacleProximity as i32,
                max_linear_speed_mps: Some(0.2),
                max_angular_speed_radps: None,
                observed_value: None,
            }],
            oldest_capture_time_nanos: Some(10),
            valid_from_nanos: 5,
            expires_at_nanos: 20,
        };
        assert_eq!(
            future_capture_limited.validate(),
            Err(ConstraintValidationError::InvalidProduct)
        );
        // Expires-at before valid-from is rejected regardless of permission.
        let expired = MotionConstraints {
            sequence: 1,
            permission: Permission::Stopped as i32,
            constraints: Vec::new(),
            oldest_capture_time_nanos: None,
            valid_from_nanos: 10,
            expires_at_nanos: 5,
        };
        assert_eq!(
            expired.validate(),
            Err(ConstraintValidationError::InvalidProduct)
        );
    }

    #[test]
    fn constraint_messages_retain_their_owner_package_identity() {
        assert_eq!(MotionConstraints::PACKAGE, "phoxal.motion.v1");
        assert_eq!(Constraint::PACKAGE, "phoxal.motion.v1");
        assert_eq!(ConstraintReason::Unspecified as i32, 0);
        assert_eq!(Permission::Stopped as i32, 3);
    }
}
