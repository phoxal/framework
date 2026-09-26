use std::collections::HashSet;

use crate::api::types::phoxal::motion::v1::{
    Constraint, ConstraintReason, ControlMode, MotionConstraints, MotionStatus, Permission,
};
use crate::api::types::phoxal::safety::v1::SafetyStatus;
use crate::api::types::phoxal::world::v1::{WorldBelief, WorldRevision};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("safety value is invalid")]
    InvalidValue,
    #[error("safety value is internally inconsistent")]
    Inconsistent,
}

pub fn capture_is_fresh_at(capture: Option<u64>, now_nanos: u64, max_age_nanos: u64) -> bool {
    capture
        .and_then(|capture| now_nanos.checked_sub(capture))
        .is_some_and(|age| age <= max_age_nanos)
}

pub fn world(value: &WorldBelief) -> Result<(), ValidationError> {
    if value.frame_id.is_empty()
        || !value.x_m.is_finite()
        || !value.y_m.is_finite()
        || !value.yaw_rad.is_finite()
        || !(-std::f64::consts::PI..=std::f64::consts::PI).contains(&value.yaw_rad)
        || !value.confidence.is_finite()
        || !(0.0..=1.0).contains(&value.confidence)
        || (value.available && value.oldest_capture_time_nanos.is_none())
    {
        return Err(ValidationError::InvalidValue);
    }
    Ok(())
}

pub fn world_revision(value: &WorldRevision) -> Result<(), ValidationError> {
    if value.available && value.oldest_capture_time_nanos.is_none() {
        return Err(ValidationError::InvalidValue);
    }
    Ok(())
}

pub fn motion_status(value: &MotionStatus) -> Result<(), ValidationError> {
    let mode = ControlMode::try_from(value.mode)
        .ok()
        .filter(|mode| *mode != ControlMode::Unspecified)
        .ok_or(ValidationError::InvalidValue)?;
    if value
        .selected_owner_id
        .as_ref()
        .is_some_and(|owner| owner.is_empty() || owner.len() > 64)
        || (mode == ControlMode::Disarmed && value.selected_owner_id.is_some())
    {
        return Err(ValidationError::Inconsistent);
    }
    Ok(())
}

fn constraint(value: &Constraint) -> Result<(), ValidationError> {
    let reason = ConstraintReason::try_from(value.reason)
        .ok()
        .filter(|reason| *reason != ConstraintReason::Unspecified)
        .ok_or(ValidationError::InvalidValue)?;
    for quantity in [
        value.max_linear_speed_mps,
        value.max_angular_speed_radps,
        value.observed_value,
    ]
    .into_iter()
    .flatten()
    {
        if !quantity.is_finite() || quantity < 0.0 {
            return Err(ValidationError::InvalidValue);
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
        return Err(ValidationError::InvalidValue);
    }
    Ok(())
}

pub fn constraints(value: &MotionConstraints) -> Result<(), ValidationError> {
    let permission = Permission::try_from(value.permission)
        .ok()
        .filter(|permission| *permission != Permission::Unspecified)
        .ok_or(ValidationError::InvalidValue)?;
    if permission != Permission::Stopped
        && value
            .oldest_capture_time_nanos
            .is_none_or(|capture| capture > value.valid_from_nanos)
    {
        return Err(ValidationError::Inconsistent);
    }
    if value.expires_at_nanos < value.valid_from_nanos || value.constraints.len() > 16 {
        return Err(ValidationError::Inconsistent);
    }
    let mut reasons = HashSet::with_capacity(value.constraints.len());
    let mut has_limit = false;
    for item in &value.constraints {
        constraint(item)?;
        let reason =
            ConstraintReason::try_from(item.reason).map_err(|_| ValidationError::InvalidValue)?;
        if !reasons.insert(reason) {
            return Err(ValidationError::Inconsistent);
        }
        if permission == Permission::Limited
            && (reason != ConstraintReason::ObstacleProximity
                || (item.max_linear_speed_mps.is_none() && item.max_angular_speed_radps.is_none()))
        {
            return Err(ValidationError::Inconsistent);
        }
        has_limit |= item.max_linear_speed_mps.is_some() || item.max_angular_speed_radps.is_some();
    }
    match permission {
        Permission::Clear if !value.constraints.is_empty() => Err(ValidationError::Inconsistent),
        Permission::Limited if value.constraints.is_empty() || !has_limit => {
            Err(ValidationError::Inconsistent)
        }
        Permission::Stopped if value.constraints.is_empty() => Err(ValidationError::Inconsistent),
        _ => Ok(()),
    }
}

pub fn status(value: &SafetyStatus) -> Result<(), ValidationError> {
    if value.reasons.len() > 16 {
        return Err(ValidationError::InvalidValue);
    }
    let mut reasons = HashSet::with_capacity(value.reasons.len());
    for reason in &value.reasons {
        let reason = ConstraintReason::try_from(*reason)
            .ok()
            .filter(|reason| *reason != ConstraintReason::Unspecified)
            .ok_or(ValidationError::InvalidValue)?;
        if !reasons.insert(reason) {
            return Err(ValidationError::InvalidValue);
        }
    }
    if value.protective_state_clear && !value.reasons.is_empty() {
        return Err(ValidationError::Inconsistent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_status_has_no_reasons() {
        assert!(
            status(&SafetyStatus {
                protective_state_clear: true,
                sequence: 1,
                reasons: Vec::new(),
            })
            .is_ok()
        );
    }
}
