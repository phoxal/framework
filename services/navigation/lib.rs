//! Generated navigation messages, typed ports, and domain validation.

use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/phoxal.navigation.v1.rs"));

/// Public typed ports owned by the Navigation Protobuf service.
pub use navigation::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// Maximum UTF-8 byte length of execution-scoped navigation identifiers.
pub const MAX_ID_BYTES: usize = 64;

/// Number of terminal results retained by one navigation instance per execution.
pub const TERMINAL_RESULT_RETENTION: usize = 256;

/// A navigation message violates the version-one domain contract.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// A required message or oneof selection is absent.
    #[error("{0} is required")]
    Missing(&'static str),
    /// A string identifier is empty or exceeds the contract bound.
    #[error("{field} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes")]
    InvalidId {
        /// The invalid field name.
        field: &'static str,
    },
    /// A coordinate or heading is not finite.
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    /// An enum contains an unspecified or unknown numeric value.
    #[error("{0} contains an unspecified or unknown value")]
    InvalidEnum(&'static str),
    /// Availability reasons contain a duplicate or contradict their owner message.
    #[error("invalid unavailable_reasons: {0}")]
    InvalidUnavailableReasons(&'static str),
    /// Navigation state fields contradict its phase or availability.
    #[error("invalid navigation state: {0}")]
    InvalidState(&'static str),
}

impl GoalTarget {
    /// Validates coordinate and frame identity invariants.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.frame_id, "frame_id")?;
        validate_finite(self.x_m, "x_m")?;
        validate_finite(self.y_m, "y_m")?;
        if let Some(heading) = self.final_heading_rad {
            validate_finite(heading, "final_heading_rad")?;
        }
        Ok(())
    }
}

impl ApplyCommandRequest {
    /// Validates the selected navigation command.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self
            .command
            .as_ref()
            .ok_or(ValidationError::Missing("command"))?
        {
            apply_command_request::Command::Start(start) => {
                validate_id(&start.goal_id, "goal_id")?;
                start
                    .target
                    .as_ref()
                    .ok_or(ValidationError::Missing("target"))?
                    .validate()
            }
            apply_command_request::Command::Cancel(cancel) => {
                validate_id(&cancel.goal_id, "goal_id")
            }
        }
    }
}

impl ApplyCommandResponse {
    /// Validates acceptance or refusal and its reason details.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self
            .decision
            .as_ref()
            .ok_or(ValidationError::Missing("decision"))?
        {
            apply_command_response::Decision::Accepted(_) => Ok(()),
            apply_command_response::Decision::Refused(refused) => refused.validate(),
        }
    }
}

impl Refused {
    fn validate(&self) -> Result<(), ValidationError> {
        let reason = RefusalReason::try_from(self.reason)
            .ok()
            .filter(|reason| *reason != RefusalReason::Unspecified)
            .ok_or(ValidationError::InvalidEnum("reason"))?;
        validate_unavailable_reasons(&self.unavailable_reasons)?;
        if reason == RefusalReason::Unavailable && self.unavailable_reasons.is_empty() {
            return Err(ValidationError::InvalidUnavailableReasons(
                "unavailable refusal requires at least one reason",
            ));
        }
        if reason != RefusalReason::Unavailable && !self.unavailable_reasons.is_empty() {
            return Err(ValidationError::InvalidUnavailableReasons(
                "only an unavailable refusal carries availability reasons",
            ));
        }
        Ok(())
    }
}

impl NavigationState {
    /// Validates phase, goal identity, and availability coherence.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let phase = Phase::try_from(self.phase)
            .ok()
            .filter(|phase| *phase != Phase::Unspecified)
            .ok_or(ValidationError::InvalidEnum("phase"))?;
        if let Some(goal_id) = &self.active_goal_id {
            validate_id(goal_id, "active_goal_id")?;
        }
        validate_unavailable_reasons(&self.unavailable_reasons)?;
        if phase == Phase::Idle && self.active_goal_id.is_some() {
            return Err(ValidationError::InvalidState(
                "idle state cannot retain an active goal",
            ));
        }
        if phase != Phase::Idle && self.active_goal_id.is_none() {
            return Err(ValidationError::InvalidState(
                "searching and following require an active goal",
            ));
        }
        if !self.unavailable_reasons.is_empty() && phase != Phase::Idle {
            return Err(ValidationError::InvalidState(
                "unavailable navigation must be idle",
            ));
        }
        Ok(())
    }
}

impl GoalFinished {
    /// Validates a terminal goal outcome and its availability details.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.goal_id, "goal_id")?;
        let outcome = GoalOutcome::try_from(self.outcome)
            .ok()
            .filter(|outcome| *outcome != GoalOutcome::Unspecified)
            .ok_or(ValidationError::InvalidEnum("outcome"))?;
        validate_unavailable_reasons(&self.unavailable_reasons)?;
        if outcome == GoalOutcome::Unavailable && self.unavailable_reasons.is_empty() {
            return Err(ValidationError::InvalidUnavailableReasons(
                "unavailable outcome requires at least one reason",
            ));
        }
        if outcome != GoalOutcome::Unavailable && !self.unavailable_reasons.is_empty() {
            return Err(ValidationError::InvalidUnavailableReasons(
                "only an unavailable outcome carries availability reasons",
            ));
        }
        Ok(())
    }
}

impl GetGoalStatusRequest {
    /// Validates the execution-scoped goal identifier.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.goal_id, "goal_id")
    }
}

impl GetGoalStatusResponse {
    /// Validates the retained goal status selection and identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self
            .status
            .as_ref()
            .ok_or(ValidationError::Missing("status"))?
        {
            get_goal_status_response::Status::Running(running) => {
                validate_id(&running.goal_id, "goal_id")
            }
            get_goal_status_response::Status::Finished(finished) => finished.validate(),
            get_goal_status_response::Status::UnknownOrNoLongerRetained(unknown) => {
                validate_id(&unknown.goal_id, "goal_id")
            }
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
    if !value.is_finite() {
        return Err(ValidationError::NonFinite(field));
    }
    Ok(())
}

fn validate_unavailable_reasons(reasons: &[i32]) -> Result<(), ValidationError> {
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
    use phoxal::port::{PortDescriptor, PortKind};
    use prost::Name;

    use super::*;

    #[test]
    fn generated_ports_retain_names_kinds_and_message_identity() {
        assert_eq!(ports::STATUS.name(), "status");
        assert_eq!(ports::COMMANDS.name(), "commands");
        assert_eq!(ports::FINISHED.name(), "finished");
        assert_eq!(ports::GET_GOAL_STATUS.name(), "get_goal_status");
        assert_eq!(
            <phoxal::port::Read<GetGoalStatusRequest, GetGoalStatusResponse> as PortDescriptor>::KIND,
            PortKind::Read
        );
        assert_eq!(NavigationState::PACKAGE, "phoxal.navigation.v1");
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }

    #[test]
    fn validates_start_goal_and_zero_coordinates() {
        ApplyCommandRequest {
            command: Some(apply_command_request::Command::Start(StartGoal {
                goal_id: "goal-1".into(),
                target: Some(GoalTarget {
                    frame_id: "map".into(),
                    x_m: 0.0,
                    y_m: -0.0,
                    final_heading_rad: None,
                }),
            })),
        }
        .validate()
        .expect("valid goal");
    }

    #[test]
    fn rejects_nonfinite_and_contradictory_values() {
        let request = ApplyCommandRequest {
            command: Some(apply_command_request::Command::Start(StartGoal {
                goal_id: "goal-1".into(),
                target: Some(GoalTarget {
                    frame_id: "map".into(),
                    x_m: f64::NAN,
                    y_m: 0.0,
                    final_heading_rad: None,
                }),
            })),
        };
        assert_eq!(request.validate(), Err(ValidationError::NonFinite("x_m")));

        let state = NavigationState {
            phase: Phase::Following.into(),
            active_goal_id: Some("goal-1".into()),
            map_revision: Some(1),
            unavailable_reasons: vec![UnavailableReason::Map.into()],
        };
        assert!(matches!(
            state.validate(),
            Err(ValidationError::InvalidState(_))
        ));
    }

    #[test]
    fn refuses_ambiguous_or_invalid_result_reconciliation() {
        assert_eq!(
            GetGoalStatusResponse { status: None }.validate(),
            Err(ValidationError::Missing("status"))
        );
        assert_eq!(TERMINAL_RESULT_RETENTION, 256);
    }
}
