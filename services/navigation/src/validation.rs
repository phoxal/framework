use std::collections::HashSet;

use crate::contract::MapState;
use crate::contract::{
    ApplyCommand, ApplyCommandResponse, GetGoalStatusRequest, GetGoalStatusResponse, GoalFinished,
    GoalOutcome, GoalTarget, NavigationState, Phase, RefusalReason, UnavailableReason,
};
use phoxal::contracts::robotics::OdometryState;

pub const MAX_ID_BYTES: usize = 64;
pub const TERMINAL_RESULT_RETENTION: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{field} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes")]
    InvalidId { field: &'static str },
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    #[error("{0} contains an unspecified or unknown value")]
    InvalidEnum(&'static str),
    #[error("invalid unavailable_reasons: {0}")]
    InvalidUnavailableReasons(&'static str),
    #[error("invalid navigation state: {0}")]
    InvalidState(&'static str),
}

pub fn capture_is_fresh_at(capture: Option<u64>, now_nanos: u64, max_age_nanos: u64) -> bool {
    capture
        .and_then(|capture| now_nanos.checked_sub(capture))
        .is_some_and(|age| age <= max_age_nanos)
}

pub fn odometry(value: &OdometryState) -> Result<(), ValidationError> {
    if value.available && value.oldest_capture_time_nanos.is_none() {
        return Err(ValidationError::Missing("oldest_capture_time_nanos"));
    }
    validate_finite(value.x_m, "x_m")?;
    validate_finite(value.y_m, "y_m")?;
    validate_finite(value.yaw_rad, "yaw_rad")?;
    validate_finite(value.linear_x_mps, "linear_x_mps")?;
    validate_finite(value.angular_z_radps, "angular_z_radps")
}

pub fn world_revision(value: &MapState) -> Result<(), ValidationError> {
    if value.available && value.oldest_capture_time_nanos.is_none() {
        return Err(ValidationError::Missing("oldest_capture_time_nanos"));
    }
    Ok(())
}

pub fn goal_target(value: &GoalTarget) -> Result<(), ValidationError> {
    validate_id(&value.frame_id, "frame_id")?;
    validate_finite(value.x_m, "x_m")?;
    validate_finite(value.y_m, "y_m")?;
    if let Some(heading) = value.final_heading_rad {
        validate_finite(heading, "final_heading_rad")?;
    }
    Ok(())
}

pub fn command_request(value: &ApplyCommand) -> Result<(), ValidationError> {
    match value {
        ApplyCommand::Start(start) => {
            validate_id(&start.goal_id, "goal_id")?;
            goal_target(
                start
                    .target
                    .as_ref()
                    .ok_or(ValidationError::Missing("target"))?,
            )
        }
        ApplyCommand::Cancel(cancel) => validate_id(&cancel.goal_id, "goal_id"),
    }
}

pub fn command_response(value: &ApplyCommandResponse) -> Result<(), ValidationError> {
    match value {
        ApplyCommandResponse::Accepted => Ok(()),
        ApplyCommandResponse::Refused(refused) => {
            if refused.reason == RefusalReason::Unspecified {
                return Err(ValidationError::InvalidEnum("reason"));
            }
            unavailable_reasons(&refused.unavailable_reasons)?;
            if refused.reason == RefusalReason::Unavailable
                && refused.unavailable_reasons.is_empty()
            {
                return Err(ValidationError::InvalidUnavailableReasons(
                    "unavailable refusal requires at least one reason",
                ));
            }
            if refused.reason != RefusalReason::Unavailable
                && !refused.unavailable_reasons.is_empty()
            {
                return Err(ValidationError::InvalidUnavailableReasons(
                    "only an unavailable refusal carries availability reasons",
                ));
            }
            Ok(())
        }
    }
}

pub fn state(value: &NavigationState) -> Result<(), ValidationError> {
    let phase = value.phase;
    if phase == Phase::Unspecified {
        return Err(ValidationError::InvalidEnum("phase"));
    }
    if let Some(goal_id) = &value.active_goal_id {
        validate_id(goal_id, "active_goal_id")?;
    }
    unavailable_reasons(&value.unavailable_reasons)?;
    if phase == Phase::Idle && value.active_goal_id.is_some() {
        return Err(ValidationError::InvalidState(
            "idle state cannot retain an active goal",
        ));
    }
    if phase != Phase::Idle && value.active_goal_id.is_none() {
        return Err(ValidationError::InvalidState(
            "searching and following require an active goal",
        ));
    }
    if !value.unavailable_reasons.is_empty() && phase != Phase::Idle {
        return Err(ValidationError::InvalidState(
            "unavailable navigation must be idle",
        ));
    }
    Ok(())
}

pub fn finished(value: &GoalFinished) -> Result<(), ValidationError> {
    validate_id(&value.goal_id, "goal_id")?;
    let outcome = value.outcome;
    if outcome == GoalOutcome::Unspecified {
        return Err(ValidationError::InvalidEnum("outcome"));
    }
    unavailable_reasons(&value.unavailable_reasons)?;
    if outcome == GoalOutcome::Unavailable && value.unavailable_reasons.is_empty() {
        return Err(ValidationError::InvalidUnavailableReasons(
            "unavailable outcome requires at least one reason",
        ));
    }
    if outcome != GoalOutcome::Unavailable && !value.unavailable_reasons.is_empty() {
        return Err(ValidationError::InvalidUnavailableReasons(
            "only an unavailable outcome carries availability reasons",
        ));
    }
    Ok(())
}

pub fn status_request(value: &GetGoalStatusRequest) -> Result<(), ValidationError> {
    validate_id(&value.goal_id, "goal_id")
}

pub fn status_response(value: &GetGoalStatusResponse) -> Result<(), ValidationError> {
    match value {
        GetGoalStatusResponse::Running(running) => validate_id(&running.goal_id, "goal_id"),
        GetGoalStatusResponse::Finished(value) => finished(value),
        GetGoalStatusResponse::UnknownOrNoLongerRetained(value) => {
            validate_id(&value.goal_id, "goal_id")
        }
    }
}

fn validate_id(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES {
        return Err(ValidationError::InvalidId { field });
    }
    Ok(())
}

fn validate_finite(value: f64, field: &'static str) -> Result<(), ValidationError> {
    value
        .is_finite()
        .then_some(())
        .ok_or(ValidationError::NonFinite(field))
}

fn unavailable_reasons(reasons: &[UnavailableReason]) -> Result<(), ValidationError> {
    if reasons.len() > 4 {
        return Err(ValidationError::InvalidUnavailableReasons(
            "at most four reasons are allowed",
        ));
    }
    let mut unique = HashSet::with_capacity(reasons.len());
    for reason in reasons {
        if *reason == UnavailableReason::Unspecified {
            return Err(ValidationError::InvalidEnum("unavailable_reasons"));
        }
        if !unique.insert(*reason) {
            return Err(ValidationError::InvalidUnavailableReasons(
                "reasons must be distinct",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_enum_variants_carry_their_own_presence() {
        // A payload enum has no absent form: absence is rejected at the
        // decoding boundary, so validation only sees selected variants.
        let running = GetGoalStatusResponse::Running(crate::contract::GoalRunning {
            goal_id: "goal".to_owned(),
        });
        assert_eq!(status_response(&running), Ok(()));
    }
}
