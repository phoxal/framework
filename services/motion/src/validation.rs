//! Private provider validation for generated Motion contract values.

use std::collections::HashSet;

use crate::api::types::phoxal::motion::v1::{
    ActuatorSetpoint, ApplyEmergencyResponse, ArmRequest, Constraint, ConstraintReason,
    ControlMode, MotionConstraints, MotionIntent, Permission, ReleaseEmergencyRequest,
    actuator_target, apply_emergency_response,
};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("invalid actuator_id: {0}")]
    InvalidActuatorId(&'static str),
    #[error("actuator {0} must select exactly one supported control")]
    MissingControl(String),
    #[error("actuator {actuator_id} {quantity} must be finite")]
    NonFinite {
        actuator_id: String,
        quantity: &'static str,
    },
    #[error("setpoint actuator membership does not match configured authority")]
    Membership,
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("reset_token must contain 1 to 64 UTF-8 bytes")]
    InvalidResetToken,
    #[error("emergency mode or refusal reason is unspecified or unknown")]
    InvalidEmergencyValue,
}

pub fn intent(value: &MotionIntent) -> Result<(), ValidationError> {
    if !value.linear_x_mps.is_finite() || !value.angular_z_radps.is_finite() {
        return Err(ValidationError::NonFinite {
            actuator_id: "intent".to_owned(),
            quantity: "intent",
        });
    }
    Ok(())
}

pub fn arm_request(value: &ArmRequest) -> Result<(), ValidationError> {
    ControlMode::try_from(value.mode)
        .ok()
        .filter(|mode| matches!(mode, ControlMode::Manual | ControlMode::Autonomous))
        .ok_or(ValidationError::InvalidEmergencyValue)?;
    Ok(())
}

pub fn release_request(value: &ReleaseEmergencyRequest) -> Result<(), ValidationError> {
    if value.reset_token.is_empty() || value.reset_token.len() > 64 {
        Err(ValidationError::InvalidResetToken)
    } else {
        Ok(())
    }
}

#[allow(
    dead_code,
    reason = "provider boundary validates responses before future external use"
)]
pub fn emergency_response(value: &ApplyEmergencyResponse) -> Result<(), ValidationError> {
    match value
        .decision
        .as_ref()
        .ok_or(ValidationError::Missing("decision"))?
    {
        apply_emergency_response::Decision::Accepted(_) => Ok(()),
        apply_emergency_response::Decision::Refused(refused) => {
            crate::api::types::phoxal::motion::v1::EmergencyRefusalReason::try_from(refused.reason)
                .ok()
                .filter(|reason| {
                    *reason != crate::api::types::phoxal::motion::v1::EmergencyRefusalReason::Unspecified
                })
                .ok_or(ValidationError::InvalidEmergencyValue)?;
            Ok(())
        }
    }
}

pub fn actuator_setpoint<'a>(
    value: &ActuatorSetpoint,
    required_actuators: impl IntoIterator<Item = &'a str>,
) -> Result<(), ValidationError> {
    let required = required_actuators.into_iter().collect::<HashSet<_>>();
    let mut actual = HashSet::with_capacity(value.targets.len());
    for target in &value.targets {
        if target.actuator_id.is_empty() {
            return Err(ValidationError::InvalidActuatorId("empty"));
        }
        if !actual.insert(target.actuator_id.as_str()) {
            return Err(ValidationError::InvalidActuatorId("duplicate"));
        }
        let quantity = match target.control.as_ref() {
            Some(actuator_target::Control::VelocityRadps(value)) => ("velocity_radps", *value),
            Some(actuator_target::Control::TorqueNm(value)) => ("torque_nm", *value),
            None => return Err(ValidationError::MissingControl(target.actuator_id.clone())),
        };
        if !quantity.1.is_finite() {
            return Err(ValidationError::NonFinite {
                actuator_id: target.actuator_id.clone(),
                quantity: quantity.0,
            });
        }
    }
    if actual != required {
        return Err(ValidationError::Membership);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConstraintValidationError {
    #[error("safety constraint is invalid")]
    InvalidConstraint,
    #[error("safety constraints are incoherent")]
    InvalidProduct,
}

fn constraint(value: &Constraint) -> Result<(), ConstraintValidationError> {
    let reason = ConstraintReason::try_from(value.reason)
        .ok()
        .filter(|reason| *reason != ConstraintReason::Unspecified)
        .ok_or(ConstraintValidationError::InvalidConstraint)?;
    for quantity in [
        value.max_linear_speed_mps,
        value.max_angular_speed_radps,
        value.observed_value,
    ]
    .into_iter()
    .flatten()
    {
        if !quantity.is_finite() || quantity < 0.0 {
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
    ) && value.max_linear_speed_mps.is_none()
        && value.max_angular_speed_radps.is_none()
        && value.observed_value.is_none()
    {
        return Err(ConstraintValidationError::InvalidConstraint);
    }
    Ok(())
}

pub fn constraints(value: &MotionConstraints) -> Result<(), ConstraintValidationError> {
    let permission = Permission::try_from(value.permission)
        .ok()
        .filter(|permission| *permission != Permission::Unspecified)
        .ok_or(ConstraintValidationError::InvalidProduct)?;
    if permission != Permission::Stopped
        && value
            .oldest_capture_time_nanos
            .is_none_or(|capture| capture > value.valid_from_nanos)
    {
        return Err(ConstraintValidationError::InvalidProduct);
    }
    if value.expires_at_nanos < value.valid_from_nanos || value.constraints.len() > 16 {
        return Err(ConstraintValidationError::InvalidProduct);
    }
    let mut reasons = HashSet::with_capacity(value.constraints.len());
    let mut has_limit = false;
    for item in &value.constraints {
        constraint(item)?;
        let reason = ConstraintReason::try_from(item.reason)
            .map_err(|_| ConstraintValidationError::InvalidConstraint)?;
        if !reasons.insert(reason) {
            return Err(ConstraintValidationError::InvalidProduct);
        }
        if permission == Permission::Limited
            && (reason != ConstraintReason::ObstacleProximity
                || (item.max_linear_speed_mps.is_none() && item.max_angular_speed_radps.is_none()))
        {
            return Err(ConstraintValidationError::InvalidProduct);
        }
        has_limit |= item.max_linear_speed_mps.is_some() || item.max_angular_speed_radps.is_some();
    }
    match permission {
        Permission::Clear if !value.constraints.is_empty() => {
            Err(ConstraintValidationError::InvalidProduct)
        }
        Permission::Limited if value.constraints.is_empty() || !has_limit => {
            Err(ConstraintValidationError::InvalidProduct)
        }
        Permission::Stopped if value.constraints.is_empty() => {
            Err(ConstraintValidationError::InvalidProduct)
        }
        _ => Ok(()),
    }
}
