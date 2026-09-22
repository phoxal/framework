use std::collections::HashSet;

use phoxal_service_kinematics::OdometryState;
use phoxal_service_navigation::{
    ApplyCommandRequest, ApplyCommandResponse, GetGoalStatusRequest, GetGoalStatusResponse,
    GoalFinished, GoalOutcome, GoalTarget, NavigationState, Phase, RefusalReason,
    UnavailableReason, apply_command_request, apply_command_response, get_goal_status_response,
};
use phoxal_service_world::WorldRevision;

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

pub fn world_revision(value: &WorldRevision) -> Result<(), ValidationError> {
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

pub fn command_request(value: &ApplyCommandRequest) -> Result<(), ValidationError> {
    match value
        .command
        .as_ref()
        .ok_or(ValidationError::Missing("command"))?
    {
        apply_command_request::Command::Start(start) => {
            validate_id(&start.goal_id, "goal_id")?;
            goal_target(
                start
                    .target
                    .as_ref()
                    .ok_or(ValidationError::Missing("target"))?,
            )
        }
        apply_command_request::Command::Cancel(cancel) => validate_id(&cancel.goal_id, "goal_id"),
    }
}

pub fn command_response(value: &ApplyCommandResponse) -> Result<(), ValidationError> {
    match value
        .decision
        .as_ref()
        .ok_or(ValidationError::Missing("decision"))?
    {
        apply_command_response::Decision::Accepted(_) => Ok(()),
        apply_command_response::Decision::Refused(refused) => {
            let reason = RefusalReason::try_from(refused.reason)
                .ok()
                .filter(|reason| *reason != RefusalReason::Unspecified)
                .ok_or(ValidationError::InvalidEnum("reason"))?;
            unavailable_reasons(&refused.unavailable_reasons)?;
            if reason == RefusalReason::Unavailable && refused.unavailable_reasons.is_empty() {
                return Err(ValidationError::InvalidUnavailableReasons(
                    "unavailable refusal requires at least one reason",
                ));
            }
            if reason != RefusalReason::Unavailable && !refused.unavailable_reasons.is_empty() {
                return Err(ValidationError::InvalidUnavailableReasons(
                    "only an unavailable refusal carries availability reasons",
                ));
            }
            Ok(())
        }
    }
}

pub fn state(value: &NavigationState) -> Result<(), ValidationError> {
    let phase = Phase::try_from(value.phase)
        .ok()
        .filter(|phase| *phase != Phase::Unspecified)
        .ok_or(ValidationError::InvalidEnum("phase"))?;
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
    let outcome = GoalOutcome::try_from(value.outcome)
        .ok()
        .filter(|outcome| *outcome != GoalOutcome::Unspecified)
        .ok_or(ValidationError::InvalidEnum("outcome"))?;
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
    match value
        .status
        .as_ref()
        .ok_or(ValidationError::Missing("status"))?
    {
        get_goal_status_response::Status::Running(running) => {
            validate_id(&running.goal_id, "goal_id")
        }
        get_goal_status_response::Status::Finished(value) => finished(value),
        get_goal_status_response::Status::UnknownOrNoLongerRetained(value) => {
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

fn unavailable_reasons(reasons: &[i32]) -> Result<(), ValidationError> {
    if reasons.len() > 4 {
        return Err(ValidationError::InvalidUnavailableReasons(
            "at most four reasons are allowed",
        ));
    }
    let mut unique = HashSet::with_capacity(reasons.len());
    for reason in reasons {
        let reason = UnavailableReason::try_from(*reason)
            .ok()
            .filter(|reason| *reason != UnavailableReason::Unspecified)
            .ok_or(ValidationError::InvalidEnum("unavailable_reasons"))?;
        if !unique.insert(reason) {
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
    fn rejects_missing_goal_status() {
        assert_eq!(
            status_response(&GetGoalStatusResponse { status: None }),
            Err(ValidationError::Missing("status"))
        );
    }
}
